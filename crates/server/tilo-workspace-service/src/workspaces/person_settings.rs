//! The person's mail accounts, calendar accounts and model providers, kept
//! by the holder their workspace runs on.
//!
//! A page has no mail connection and no vault, so the panes that add a mail
//! account or a provider key from a browser ask the holder: it opens the mail
//! server's session to prove the sign-in, keeps the account and the key in
//! the person's own vault, and answers what it kept. A secret travels in and
//! never back — a reading says only whether one is kept.
//!
//! They are the person's, not any workspace's: one store per person under
//! `people/<person>` in the holder's state, whichever of their workspaces
//! they ask from, with its own owner-only key file where there is no keyring.
//! The stores are the shared account and settings formats, so a device that
//! reads them keeps reading the same files.
//!
//! Only the person themselves asks: a device of theirs, its certificate
//! naming them. Another member, an administrator or the workspace's owner
//! included, reaches only their own store, and an agent's request is refused
//! here, as the holder's own operations refuse one.

mod sending;
pub mod turn_mail;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value};
use tilo_account_probe::{AccountTester, LiveAccountTester};
use tilo_actions::remote::person_accounts::{
    AccountAssistants, ArrivedAfter, AssistantAccess, FetchMail, FetchedMail, FetchedMessage,
    FoundMail, MarkMail, ReadMail, SearchMail,
};
use tilo_actions::remote::person_accounts::{
    AddMailAccount, BeginSend, MAX_SEND_FILE_BYTES, MAX_SEND_FILES, MAX_SEND_PIECE_BYTES,
    MAX_SEND_TOTAL_BYTES, MailAccount, MailAccounts, PieceKept, SendFile, SendId, SendPiece,
    SentMail, StartedSend,
};
// A message as text is read for sending in one place, `Leaving::read`; only
// this file's tests still name one.
#[cfg(test)]
use tilo_actions::remote::person_accounts::SendMail;
use tilo_actions::remote::person_models::{
    Assignment, ModelProviders, OfferedModel, ProviderView, SetAssignment, SetKey,
};
use tilo_actions::remote::{
    ActionCallError, Caller, InvokeArgument, InvokeRequest, person_accounts, person_models,
};
use tilo_mail_session::{Attachment, Leaving};
use tilo_net::proxy::host::Destinations;
use tilo_server_protocol::certify::{ConfirmationAnswer, PendingConfirmation};
use tilo_user_identity::DeviceCert;

use super::person_models::kept_location;
use super::{Workspaces, system_time};

/// A person's calendars, kept beside their mail accounts.
#[cfg(feature = "agents")]
pub(in crate::workspaces) mod calendars;

impl Workspaces {
    /// Where this host keeps `person`'s own accounts and model providers,
    /// with the vault their sign-ins and keys are sealed in: theirs alone,
    /// apart from every other person's, and the same in each of their
    /// workspaces.
    pub(super) fn person_settings_root(&self, person: &[u8; 32]) -> PathBuf {
        self.inner.dir.join("people").join(hex::encode(person))
    }

    /// The mail accounts the person's vault keeps, as [`person_accounts::LIST`]
    /// answers them: what a window following [`person_accounts::WATCH`] is
    /// sent again whenever they change.
    pub(super) fn accounts_listing(&self, person: &[u8; 32]) -> Result<Value, ActionCallError> {
        answer(&mail_accounts(&self.person_settings_root(person)))
    }

    /// What the person's own settings kept here changed: a window following
    /// them reads them again.
    pub(super) fn person_settings_changed(&self) {
        self.inner
            .person_settings_moved
            .send_modify(|count| *count = count.wrapping_add(1));
        // An inbox somebody listens for is watched with the accounts as its
        // person keeps them now.
        self.watch_listened_inboxes();
    }

    /// Fires whenever a person's own settings kept here change.
    pub(super) fn person_settings_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.person_settings_moved.subscribe()
    }

    /// The sessions this host holds with the mail servers of the accounts
    /// its people keep: one for the whole host, so an account's signed-in
    /// session serves its next request too.
    pub(super) fn mail_session(&self) -> Session {
        Arc::clone(
            &self
                .inner
                .mail_session
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Carry the accounts' mail through `session` from now on: a test stands
    /// in for the mail servers.
    #[cfg(any(test, feature = "test-support"))]
    pub fn carry_mail_through(&self, session: Session) {
        *self
            .inner
            .mail_session
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = session;
    }

    /// Keep the mail account `address` for `person` with a sign-in, as
    /// adding it from a window keeps one, and name the agent `named` (a
    /// workspace and the agent's id there) for it when one is given: a
    /// test stands in for the account's servers
    /// ([`Self::carry_mail_through`]).
    ///
    /// # Errors
    /// The account, its sign-in or the naming could not be kept.
    #[cfg(any(test, feature = "test-support"))]
    pub fn keep_mail_account_as_added(
        &self,
        person: &[u8; 32],
        address: &str,
        named: Option<(&str, &str)>,
    ) -> Result<(), String> {
        let root = self.person_settings_root(person);
        let id = accounts::AccountId::from(address.to_owned());
        let mut account = accounts::Account::new(id, accounts::email::TYPE_ID);
        // Where its servers are is not kept: the stand-in is every server.
        account.config = serde_json::json!({"email": address, "username": address});
        let mut store = accounts::AccountStore::load_in(&root);
        store.upsert(account.clone());
        store.save_in(&root).map_err(|error| format!("{error:?}"))?;
        let mut vault =
            accounts::AccountCredentials::open_in(&root).map_err(|error| format!("{error:?}"))?;
        // Added by its person with its password: bound to the servers it
        // names, as an added account is.
        vault.put_for(&account, &accounts::Secret::password("kept-for-a-test"));
        vault.try_save().map_err(|error| format!("{error:?}"))?;
        if let Some((workspace, agent)) = named {
            let naming = tilo_actions::remote::person_accounts::AssistantAccess {
                account: address.to_owned(),
                agent: agent.to_owned(),
                may: true,
            };
            say_assistant(&root, workspace, &naming)?;
        }
        self.person_settings_changed();
        Ok(())
    }

    /// The accounts of `person` that they named the agent `agent` of
    /// `workspace` for: what that agent may read, search and draft through.
    pub(super) fn accounts_named_for(
        &self,
        person: &[u8; 32],
        workspace: &str,
        agent: &str,
    ) -> BTreeSet<String> {
        accounts_of_assistant(
            &self.person_settings_root(person),
            NamedAgent { workspace, agent },
        )
    }

    /// What proves an account's sign-in at its server before a use by
    /// reference keeps the account.
    pub(super) fn account_tester(&self) -> Arc<dyn AccountTester> {
        Arc::clone(
            &self
                .inner
                .vault_uses
                .prover
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Prove the sign-ins of accounts added by reference through `prove`
    /// from now on, which is told the account and the password it would
    /// sign in with: a test stands in for the mail servers.
    #[cfg(any(test, feature = "test-support"))]
    pub fn sign_in_accounts_through(
        &self,
        prove: impl Fn(&str, &str) -> Result<String, String> + Send + Sync + 'static,
    ) {
        struct Proving<F>(F);
        impl<F: Fn(&str, &str) -> Result<String, String> + Send + Sync> AccountTester for Proving<F> {
            fn test(
                &self,
                account: &accounts::Account,
                secret: &accounts::Secret,
            ) -> Result<String, String> {
                (self.0)(account.id.as_str(), &secret.password)
            }
        }
        *self
            .inner
            .vault_uses
            .prover
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(Proving(prove));
    }

    /// The person `caller` asks for, on the device `certificate` certifies,
    /// from a workspace `workspace_id` names: what a request that keeps a
    /// person's own secrets needs before it runs. Unknown and forbidden are
    /// the same answer.
    pub(super) fn asking_person(
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        workspace_id: Option<collab_types::WorkspaceId>,
    ) -> Result<[u8; 32], ActionCallError> {
        workspace_id.ok_or(ActionCallError::UnknownAction)?;
        if !caller.acting().is_person() || caller.acting().app().is_some() {
            return Err(ActionCallError::UnknownAction);
        }
        let certificate = certificate
            .filter(|certificate| {
                caller.device_public_key() == Some(&certificate.device_public_key)
                    && caller.user_public_key() == Some(&certificate.user_public_key)
            })
            .ok_or(ActionCallError::UnknownAction)?;
        Ok(certificate.user_public_key)
    }

    /// Who `caller` is to a person's mail: the person on a device of theirs
    /// ([`Self::asking_person`]), or an agent acting for a person. An agent
    /// is a person's when a key of that person's stands behind its
    /// connection, as the transport certified: a device of theirs that
    /// handed it its session, a key they made for it, or the device this
    /// host runs their agent's turn for. What it may then do is what that
    /// person named it for in the workspace it asks through, and nobody
    /// else's agent is anybody here.
    pub(super) fn asking_about_mail(
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        workspace_id: Option<collab_types::WorkspaceId>,
    ) -> Result<AskingAboutMail, ActionCallError> {
        if let Ok(person) = Self::asking_person(caller, certificate, workspace_id) {
            return Ok(AskingAboutMail::Person(person));
        }
        let workspace = workspace_id.ok_or(ActionCallError::UnknownAction)?;
        let acting = caller.acting();
        let (Some(agent), Some(person)) = (acting.agent_id(), acting.acted_for_person()) else {
            return Err(ActionCallError::UnknownAction);
        };
        let person: [u8; 32] = hex::decode(person)
            .ok()
            .and_then(|key| key.try_into().ok())
            .ok_or(ActionCallError::UnknownAction)?;
        let certified = certificate.is_some_and(|certificate| {
            certificate.user_public_key == person
                && caller.device_public_key() == Some(&certificate.device_public_key)
                && caller.user_public_key() == Some(&certificate.user_public_key)
        });
        if !certified {
            return Err(ActionCallError::UnknownAction);
        }
        Ok(AskingAboutMail::Assistant {
            person,
            agent: agent.to_owned(),
            workspace: workspace.to_string(),
        })
    }

    /// Whether `caller` is an agent its person named for at least one of
    /// their mail accounts: what the catalogue offers the mail actions by.
    pub(super) fn reads_mail_as_assistant(
        &self,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        workspace_id: Option<collab_types::WorkspaceId>,
    ) -> bool {
        match Self::asking_about_mail(caller, certificate, workspace_id) {
            Ok(AskingAboutMail::Assistant {
                person,
                agent,
                workspace,
            }) => {
                let named = NamedAgent {
                    workspace: &workspace,
                    agent: &agent,
                };
                // Named for a mail account: one named for a calendar alone
                // is offered nothing of mail.
                let root = self.person_settings_root(&person);
                let kept = accounts::AccountStore::load_in(&root);
                accounts_of_assistant(&root, named).iter().any(|account| {
                    kept.get(&accounts::AccountId::from(account.clone()))
                        .is_some_and(|kept| kept.type_id.as_str() == accounts::email::TYPE_ID)
                })
            }
            _ => false,
        }
    }

    /// Answer an agent of `person`'s: what the person let it do with the
    /// accounts they named it for, and nothing else. It reads, searches and
    /// drafts. A message it would send is put to its person first. What
    /// only the person does (adding and removing accounts, marking mail,
    /// saying who may read) is not there for it: unknown and forbidden are
    /// the same answer.
    async fn assistant_invoke(
        &self,
        person: [u8; 32],
        agent: NamedAgent<'_>,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        let root = self.person_settings_root(&person);
        let named = accounts_of_assistant(&root, agent);
        // An account it is named for, whose sign-in says the servers it was
        // typed for: an agent's call binds none.
        let may = |account: &str| {
            if named.contains(account) {
                bound_for_an_agent(&root, account)
                    .map_err(|reason| ActionCallError::Refused { reason })
            } else {
                Err(ActionCallError::UnknownAction)
            }
        };
        if named.is_empty() {
            return Err(ActionCallError::UnknownAction);
        }
        match request.target.address.action.as_str() {
            person_accounts::LIST => answer(&mail_accounts(&root).map(|mut listed| {
                listed
                    .accounts
                    .retain(|account| named.contains(&account.id));
                if !named.contains(&listed.active) {
                    listed.active = String::new();
                }
                listed
            })),
            person_accounts::FETCH => {
                let asking: FetchMail = argument(request, "a folder to read")?;
                may(&asking.account)?;
                answer(&fetch_with(self.mail_session(), &root, asking).await)
            }
            person_accounts::READ => {
                let reading: ReadMail = argument(request, "a message to read")?;
                may(&reading.account)?;
                answer(&read_with(self.mail_session(), &root, reading).await)
            }
            person_accounts::SEARCH => {
                let searching: SearchMail = argument(request, "what to search for")?;
                may(&searching.account)?;
                answer(&search_with(self.mail_session(), &root, searching).await)
            }
            person_accounts::ARRIVED => {
                let mut arrived = self.arrived_after(&person, arrived_argument(request)?.after);
                arrived
                    .arrivals
                    .retain(|arrival| named.contains(&arrival.account));
                answer(&Ok::<_, String>(arrived))
            }
            person_accounts::BEGIN_SEND => {
                let beginning: BeginSend = argument(request, "a message to begin")?;
                may(&beginning.message.account)?;
                answer(&begin_send(&root, &beginning))
            }
            person_accounts::SEND_PIECE => {
                let piece: SendPiece = argument(request, "a piece of a message's file")?;
                may(&begun_for(&root, &piece.send)?.1.message.account)?;
                answer(&send_piece(&root, &piece))
            }
            person_accounts::SAVE_DRAFT => {
                let keeping: SendId = argument(request, "a draft to keep")?;
                may(&begun_for(&root, &keeping.send)?.1.message.account)?;
                answer(&save_staged_draft_with(self.mail_session(), &root, keeping).await)
            }
            person_accounts::SEND => {
                let sending: SendId = argument(request, "a message to send")?;
                let (dir, beginning) = begun_for(&root, &sending.send)?;
                may(&beginning.message.account)?;
                // The message is read once, here. Its person is shown that
                // reading, and that reading is what leaves on their yes:
                // nothing is read again from what was begun.
                let read =
                    Staged::read(dir, beginning).and_then(|staged| Ok((staged.shown()?, staged)));
                let sent = match read {
                    Ok((shown, staged)) => {
                        // A message that left already is answered as the send
                        // it was: its person is asked nothing more.
                        if let Some(sent) = self
                            .recorded_mail_send(Some(agent.workspace), &staged)
                            .await
                        {
                            Ok(sent)
                        } else {
                            self.ask_before_sending(person, agent.agent, caller, &shown)
                                .await?;
                            self.send_in_workspace(
                                Some(agent.workspace),
                                self.mail_session(),
                                &root,
                                staged,
                            )
                            .await
                        }
                    }
                    Err(unshowable) => Err(unshowable),
                };
                answer(&sent)
            }
            _ => Err(ActionCallError::UnknownAction),
        }
    }

    /// Put a message an agent would send to its person and wait for their
    /// answer: only their allowing it lets it leave. A host with nobody to
    /// ask through, and a question nobody answered, say that the person's
    /// confirmation is still wanted.
    async fn ask_before_sending(
        &self,
        person: [u8; 32],
        agent: &str,
        caller: &Caller,
        shown: &sending::Shown,
    ) -> Result<(), ActionCallError> {
        let asker = self
            .inner
            .asks_people
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .ok_or_else(ActionCallError::confirmation_required)?;
        match asker.ask(person, shown.question(agent, caller)).await {
            Ok(ConfirmationAnswer::AllowOnce) => Ok(()),
            Ok(ConfirmationAnswer::Deny) => Err(ActionCallError::Refused {
                reason: "the person did not allow it".into(),
            }),
            Err(_) => Err(ActionCallError::confirmation_required()),
        }
    }

    /// Answer a [`person_accounts::PROVIDER`] request.
    pub(super) async fn person_accounts_invoke(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        // The person's calendars are another resource of the same provider.
        #[cfg(feature = "agents")]
        if request.target.address.resource == calendars::RESOURCE {
            return self
                .person_calendars_invoke(workspace_id, caller, certificate, request)
                .await;
        }
        let person = match Self::asking_about_mail(caller, certificate, workspace_id)? {
            AskingAboutMail::Person(person) => person,
            AskingAboutMail::Assistant {
                person,
                agent,
                workspace,
            } => {
                let named = NamedAgent {
                    workspace: &workspace,
                    agent: &agent,
                };
                return self.assistant_invoke(person, named, caller, request).await;
            }
        };
        let root = self.person_settings_root(&person);
        // The person's request came through a workspace, or it was not
        // theirs: an agent they name is named in that one.
        let here = workspace_id.map(|id| id.to_string()).unwrap_or_default();
        // The person asks the keeper of their accounts to sign in: a sign-in
        // kept with no servers on it is bound to the servers its account
        // names now.
        if signs_in(request.target.address.action.as_str())
            && let Err(reason) = bind_kept_first(&root)
        {
            return Err(ActionCallError::Refused { reason });
        }
        match request.target.address.action.as_str() {
            person_accounts::LIST => answer(&mail_accounts(&root)),
            person_accounts::ADD_MAIL => {
                let adding: AddMailAccount = argument(request, "an account to add")?;
                let kept = add_mail(&root, adding).await;
                if kept.is_ok() {
                    self.person_settings_changed();
                }
                answer(&kept)
            }
            person_accounts::REMOVE => {
                let account = account_argument(request)?;
                let kept = remove_mail(&root, &account);
                if kept.is_ok() {
                    self.person_settings_changed();
                }
                answer(&kept)
            }
            person_accounts::FETCH => {
                let asking: FetchMail = argument(request, "a folder to read")?;
                answer(&fetch_with(self.mail_session(), &root, asking).await)
            }
            person_accounts::READ => {
                let reading: ReadMail = argument(request, "a message to read")?;
                answer(&read_with(self.mail_session(), &root, reading).await)
            }
            person_accounts::SEARCH => {
                let searching: SearchMail = argument(request, "what to search for")?;
                answer(&search_with(self.mail_session(), &root, searching).await)
            }
            person_accounts::SEND => {
                let sending: SendId = argument(request, "a message to send")?;
                answer(&self.send_a_message(&here, &root, sending).await)
            }
            person_accounts::SAVE_DRAFT => {
                let keeping: SendId = argument(request, "a draft to keep")?;
                answer(&save_staged_draft_with(self.mail_session(), &root, keeping).await)
            }
            person_accounts::MARK => {
                let marking: MarkMail = argument(request, "what changed about a message")?;
                answer(&mark_with(self.mail_session(), &root, marking).await)
            }
            person_accounts::ASSISTANT => {
                let saying: AssistantAccess = argument(request, "who may read an account")?;
                answer(&say_assistant(&root, &here, &saying))
            }
            person_accounts::ASSISTANTS => {
                let account = account_argument(request)?;
                answer(&Ok::<_, String>(account_assistants(&root, &here, &account)))
            }
            person_accounts::ARRIVED => {
                let asking = arrived_argument(request)?;
                answer(&Ok::<_, String>(self.arrived_after(&person, asking.after)))
            }
            person_accounts::BEGIN_SEND => {
                let beginning: BeginSend = argument(request, "a message to begin")?;
                answer(&begin_send(&root, &beginning))
            }
            person_accounts::SEND_PIECE => {
                let piece: SendPiece = argument(request, "a piece of a message's file")?;
                answer(&send_piece(&root, &piece))
            }
            _ => Err(ActionCallError::UnknownAction),
        }
    }
}

/// Turn one action's answer into what the caller reads, or the reason it was
/// refused.
pub(super) fn answer<T: serde::Serialize>(
    outcome: &Result<T, String>,
) -> Result<Value, ActionCallError> {
    match outcome {
        Ok(value) => serde_json::to_value(value).map_err(|error| ActionCallError::Refused {
            reason: error.to_string(),
        }),
        Err(reason) => Err(ActionCallError::Refused {
            reason: reason.clone(),
        }),
    }
}

fn invalid(reason: &str) -> ActionCallError {
    ActionCallError::InvalidRequest {
        reason: reason.to_owned(),
    }
}

/// The arguments of a request, as the value its action names.
pub(super) fn argument<T: serde::de::DeserializeOwned>(
    request: &InvokeRequest,
    what: &str,
) -> Result<T, ActionCallError> {
    let InvokeArgument::Json(argument) = &request.argument else {
        return Err(invalid(&format!("a request names {what}")));
    };
    serde_json::from_value(Value::Object(argument.clone()))
        .map_err(|error| invalid(&format!("{what} is not readable: {error}")))
}

/// The `account` a request about one account names.
fn account_argument(request: &InvokeRequest) -> Result<String, ActionCallError> {
    let value: Map<String, Value> = argument(request, "the account")?;
    value
        .get("account")
        .and_then(Value::as_str)
        .filter(|account| !account.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| invalid("the request names its account"))
}

/// The place in the arrival log a [`person_accounts::ARRIVED`] reads on
/// from: its start, when the request names none.
fn arrived_argument(request: &InvokeRequest) -> Result<ArrivedAfter, ActionCallError> {
    match &request.argument {
        InvokeArgument::Json(_) => argument(request, "a place in the arrival log"),
        _ => Ok(ArrivedAfter::default()),
    }
}

/// The message begun under `send` and where its files wait, for a request
/// that goes on with it.
fn begun_for(root: &Path, send: &str) -> Result<(PathBuf, BeginSend), ActionCallError> {
    begun_send(root, send).map_err(|reason| ActionCallError::Refused { reason })
}

/// Who asks about a person's mail.
pub(super) enum AskingAboutMail {
    /// The person, on a device of theirs.
    Person([u8; 32]),
    /// An agent acting for the person, by the id it acts under in the
    /// workspace its request came through.
    Assistant {
        person: [u8; 32],
        agent: String,
        workspace: String,
    },
}

/// An agent as its person names it for their mail: by the id it acts under
/// and the workspace it acts in. An id means something among one
/// workspace's agents alone, so the same id in another workspace, where
/// somebody else may have made the agent, names nobody.
#[derive(Clone, Copy)]
struct NamedAgent<'a> {
    workspace: &'a str,
    agent: &'a str,
}

/// The `provider` a removing action names.
pub(super) fn provider_argument(request: &InvokeRequest) -> Result<String, ActionCallError> {
    let value: Map<String, Value> = argument(request, "the provider")?;
    value
        .get("provider")
        .and_then(Value::as_str)
        .filter(|provider| !provider.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| invalid("the provider is named by its id"))
}

/// The person's own vault, every sign-in and key of theirs sealed in it.
pub(super) fn credentials(root: &Path) -> Result<accounts::AccountCredentials, String> {
    accounts::AccountCredentials::open_in(root)
}

/// The mail accounts the person keeps, without their secrets.
fn mail_accounts(root: &Path) -> Result<MailAccounts, String> {
    let store = accounts::AccountStore::load_in(root);
    let vault = credentials(root)?;
    Ok(MailAccounts {
        accounts: store
            .list(&accounts::AccountTypeId::from(accounts::email::TYPE_ID))
            .into_iter()
            .map(|account| MailAccount {
                id: account.id.to_string(),
                address: account
                    .config
                    .get("email")
                    .and_then(Value::as_str)
                    .unwrap_or(account.id.as_str())
                    .to_owned(),
                display_name: account.display_name.clone(),
                incoming_host: field(account, "imap_host"),
                incoming_port: port(account, "imap_port", 993),
                outgoing_host: field(account, "smtp_host"),
                outgoing_port: port(account, "smtp_port", 465),
                has_secret: vault
                    .get_for(account)
                    .is_some_and(|secret| !secret.password.is_empty()),
            })
            .collect(),
        active: store
            .active_id(&accounts::AccountTypeId::from(accounts::email::TYPE_ID))
            .map(ToString::to_string)
            .unwrap_or_default(),
    })
}

fn field(account: &accounts::Account, name: &str) -> String {
    account
        .config
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn port(account: &accounts::Account, name: &str, default: u16) -> u16 {
    account
        .config
        .get(name)
        .and_then(Value::as_u64)
        .and_then(|port| u16::try_from(port).ok())
        .unwrap_or(default)
}

/// Add `adding` once its server accepted its sign-in: the secret into the
/// person's vault first, then the account into the shared store, so an
/// account never shows without the sign-in it needs.
async fn add_mail(root: &Path, adding: AddMailAccount) -> Result<MailAccount, String> {
    add_mail_with(Arc::new(LiveAccountTester), root, adding).await
}

/// [`add_mail`] through `tester`, so a test proves the store's side without
/// a server.
pub(super) async fn add_mail_with(
    tester: Arc<dyn AccountTester>,
    root: &Path,
    adding: AddMailAccount,
) -> Result<MailAccount, String> {
    let config = accounts::email::AccountConfig {
        display_name: adding.display_name.trim().to_owned(),
        username: adding
            .username
            .filter(|username| !username.trim().is_empty())
            .unwrap_or_else(|| adding.address.trim().to_owned()),
        email: adding.address.trim().to_owned(),
        imap_host: adding.incoming_host.trim().to_owned(),
        imap_port: adding.incoming_port,
        // The port says how the connection starts: 143 upgrades with
        // STARTTLS, every other port is TLS from the first byte.
        imap_tls: accounts::email::Security::of_imap(true, adding.incoming_port).wraps(),
        smtp_host: adding.outgoing_host.trim().to_owned(),
        smtp_port: adding.outgoing_port,
        smtp_tls: true,
        auth: accounts::email::AuthMethod::Password,
        // An account another mail program moved in arrives with its servers
        // and its identities. With the sign-in that program saved it is
        // proved and kept at once; without one it is kept waiting for the
        // sign-in its person completes in Settings.
        import_pending: adding.password.is_empty(),
        password: String::new(),
        identities: accounts::email::sender_identities(adding.identities)?,
    };
    if let Some((_, problem)) = config.validate().first() {
        return Err(problem.clone());
    }
    let mut account = accounts::Account::new(
        accounts::AccountId::from(config.email.clone()),
        accounts::email::TYPE_ID,
    );
    if config.display_name.is_empty() {
        config.email.clone_into(&mut account.display_name);
    } else {
        config.display_name.clone_into(&mut account.display_name);
    }
    account.config = config.to_value().map_err(|error| error.to_string())?;

    if !adding.password.is_empty() {
        let checking = account.clone();
        let secret = accounts::Secret::password(adding.password.clone());
        tokio::task::spawn_blocking(move || tester.test(&checking, &secret))
            .await
            .map_err(|_| "the mail server check did not finish".to_string())??;
        let mut vault = credentials(root)?;
        vault.put_for(&account, &accounts::Secret::password(adding.password));
        vault.try_save()?;
    }
    let mut store = accounts::AccountStore::load_in(root);
    store.upsert(account.clone());
    store.save_in(root).map_err(|error| error.to_string())?;
    let kept = mail_accounts(root)?;
    kept.accounts
        .into_iter()
        .find(|kept| kept.id == account.id.as_str())
        .ok_or_else(|| "the account was kept but did not read back".to_owned())
}

/// Remove `account` and its secret together.
fn remove_mail(root: &Path, account: &str) -> Result<MailAccounts, String> {
    let mut store = accounts::AccountStore::load_in(root);
    let id = accounts::AccountId::from(account.to_owned());
    if !store.remove(&id) {
        return Err("That account is not kept here".into());
    }
    store.save_in(root).map_err(|error| error.to_string())?;
    let mut vault = credentials(root)?;
    if vault.remove(&id) {
        vault.try_save()?;
    }
    // Nobody reads an account that is gone.
    let mut named = assistants(root);
    if named.remove(account).is_some() {
        write_assistants(root, &named)?;
    }
    mail_accounts(root)
}

/// Where a person's word on which agents of theirs may read which of their
/// accounts is kept: in their own store, beside the accounts.
fn assistants_at(root: &Path) -> PathBuf {
    root.join("mail").join("assistants.json")
}

/// The agents a person let read, search and draft through each of their
/// accounts: by the account's id, then by the workspace each agent was
/// named in.
type Assistants = BTreeMap<String, BTreeMap<String, BTreeSet<String>>>;

/// Who a person named for their accounts. What does not read names nobody.
fn assistants(root: &Path) -> Assistants {
    let bytes = match std::fs::read(assistants_at(root)) {
        Ok(bytes) => bytes,
        // The person named nobody yet.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return BTreeMap::new(),
        Err(error) => {
            tracing::warn!(%error, "mail: who may read a person's accounts could not be read, so no agent may");
            return BTreeMap::new();
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(named) => named,
        Err(error) => {
            tracing::warn!(%error, "mail: who may read a person's accounts does not read, so no agent may");
            BTreeMap::new()
        }
    }
}

fn write_assistants(root: &Path, named: &Assistants) -> Result<(), String> {
    let bytes = serde_json::to_vec(named).map_err(|error| error.to_string())?;
    tilo_files::native::atomic::write_creating_parent_with_mode(&assistants_at(root), &bytes, 0o600)
        .map_err(|error| error.to_string())
}

/// The accounts `named` may read, search and draft through.
fn accounts_of_assistant(root: &Path, named: NamedAgent<'_>) -> BTreeSet<String> {
    assistants(root)
        .into_iter()
        .filter(|(_, workspaces)| {
            workspaces
                .get(named.workspace)
                .is_some_and(|agents| agents.contains(named.agent))
        })
        .map(|(account, _)| account)
        .collect()
}

/// The agents of `workspace` that may read, search and draft through
/// `account`.
fn account_assistants(root: &Path, workspace: &str, account: &str) -> AccountAssistants {
    AccountAssistants {
        account: account.to_owned(),
        agents: assistants(root)
            .remove(account)
            .and_then(|mut workspaces| workspaces.remove(workspace))
            .unwrap_or_default()
            .into_iter()
            .collect(),
    }
}

/// Keep the person's word on one agent of `workspace` and one account they
/// keep here, and answer who of that workspace may read the account now.
fn say_assistant(
    root: &Path,
    workspace: &str,
    saying: &AssistantAccess,
) -> Result<AccountAssistants, String> {
    let agent = saying.agent.trim();
    if agent.is_empty() || agent.chars().count() > 256 || agent.chars().any(char::is_control) {
        return Err("an agent is named by the id it acts under".into());
    }
    let kept_here = accounts::AccountStore::load_in(root)
        .get(&accounts::AccountId::from(saying.account.clone()))
        .is_some_and(|account| {
            matches!(
                account.type_id.as_str(),
                accounts::email::TYPE_ID | "caldav"
            )
        });
    if !kept_here {
        return Err("That account is not kept here".into());
    }
    let mut named = assistants(root);
    let workspaces = named.entry(saying.account.clone()).or_default();
    let agents = workspaces.entry(workspace.to_owned()).or_default();
    if saying.may {
        agents.insert(agent.to_owned());
    } else {
        agents.remove(agent);
    }
    if agents.is_empty() {
        workspaces.remove(workspace);
    }
    if workspaces.is_empty() {
        named.remove(&saying.account);
    }
    write_assistants(root, &named)?;
    Ok(account_assistants(root, workspace, &saying.account))
}

/// The account `id` names, with the sign-in its vault keeps: what a page
/// cannot read for itself, and what the host opens the server's session
/// with.
fn account_with_secret(
    root: &Path,
    id: &str,
) -> Result<(accounts::Account, accounts::Secret), String> {
    let store = accounts::AccountStore::load_in(root);
    let account = store
        .get(&accounts::AccountId::from(id.to_owned()))
        .filter(|account| account.type_id.as_str() == accounts::email::TYPE_ID)
        .cloned()
        .ok_or_else(|| "That account is not kept here".to_owned())?;
    // Its sign-in was typed for the servers it named then, and is joined to
    // it only while it names those.
    let vault = credentials(root)?;
    let secret = vault
        .get_for(&account)
        .ok_or_else(|| "That account has no sign-in yet".to_owned())?;
    Ok((account, secret))
}

/// Whether `action`, asked by a person themselves, signs in to one of their
/// mail accounts.
fn signs_in(action: &str) -> bool {
    matches!(
        action,
        person_accounts::FETCH
            | person_accounts::READ
            | person_accounts::SEARCH
            | person_accounts::SEND
            | person_accounts::SAVE_DRAFT
            | person_accounts::MARK
    )
}

/// Bind each mail sign-in kept under `root` with no servers on it to the
/// servers its account names now. Asked by the keeper of the accounts when
/// it signs in: for the person themselves, and for this server's own
/// watching of their inboxes. Never on a call an agent makes under its own
/// name ([`bound_for_an_agent`]).
pub(super) fn bind_kept_first(root: &Path) -> Result<(), String> {
    let store = accounts::AccountStore::load_in(root);
    credentials(root)?
        .bind_first(store.list(&accounts::AccountTypeId::from(accounts::email::TYPE_ID)))
}

/// Whether the sign-in kept for `account` says the servers it was typed
/// for, so that an agent's call may use it: a call an agent makes under its
/// own name binds none.
fn bound_for_an_agent(root: &Path, account: &str) -> Result<(), String> {
    let vault = credentials(root)?;
    let id = accounts::AccountId::from(account.to_owned());
    if vault.get(&id).is_some() && vault.typed_for(&id).is_none() {
        return Err(
            "That account's sign-in is not bound to its server yet: it is bound when its person reads their mail themselves"
                .to_owned(),
        );
    }
    Ok(())
}

/// Where a begun message waits between its pieces and its send: one
/// directory per message, holding what it says, whom it goes to and the
/// files' bytes as they arrive. It is gone once the message is sent.
fn sends_root(root: &Path) -> PathBuf {
    root.join("sends")
}

/// How long a begun message waits for its pieces before it is forgotten: a
/// send is a page's errand of a minute, and a page that went away leaves
/// nothing behind.
const SEND_WAIT: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Forget every begun message older than [`SEND_WAIT`], under `root`.
fn forget_old_sends(root: &Path) {
    let Ok(entries) = std::fs::read_dir(sends_root(root)) else {
        return;
    };
    let now = tilo_clock::wall();
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|written| now.duration_since(written).ok())
            .is_some_and(|age| age > SEND_WAIT);
        if old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Why `beginning` is no message a host begins, if it is not.
fn check_beginning(beginning: &BeginSend) -> Result<(), String> {
    if beginning.files.len() > MAX_SEND_FILES {
        return Err(format!(
            "a message carries at most {MAX_SEND_FILES} files; this one names {}",
            beginning.files.len()
        ));
    }
    let mut total = 0u64;
    for file in &beginning.files {
        let usable = |text: &str, allow: &[char]| {
            !text.is_empty() && !text.chars().any(char::is_control) && !text.contains(allow)
        };
        if !usable(&file.filename, &['/', '\\']) {
            return Err(format!("{:?} is no name to attach", file.filename));
        }
        if !file.mime.contains('/') || !usable(&file.mime, &['"', '\\']) {
            return Err(format!("{:?} is no type to attach", file.mime));
        }
        if file.size > MAX_SEND_FILE_BYTES {
            return Err(format!(
                "{} is larger than the {MAX_SEND_FILE_BYTES} bytes one file may be",
                file.filename
            ));
        }
        total = total
            .checked_add(file.size)
            .ok_or_else(|| "the message's files are too large".to_string())?;
    }
    if total > MAX_SEND_TOTAL_BYTES {
        return Err(format!(
            "the message's files are larger than the {MAX_SEND_TOTAL_BYTES} bytes one message may carry"
        ));
    }
    if beginning
        .calendar_ics
        .as_ref()
        .is_some_and(|ics| u64::try_from(ics.len()).map_or(true, |size| size > MAX_SEND_FILE_BYTES))
    {
        return Err("the calendar invitation is too large".into());
    }
    Ok(())
}

/// The begun message called `send` under `root`, as it was begun.
fn begun_send(root: &Path, send: &str) -> Result<(PathBuf, BeginSend), String> {
    if send.is_empty() || !send.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("That message is no longer being sent".into());
    }
    let dir = sends_root(root).join(send);
    let meta = std::fs::read(dir.join("meta.json"))
        .map_err(|_| "That message is no longer being sent".to_owned())?;
    let beginning: BeginSend = serde_json::from_slice(&meta)
        .map_err(|_| "That message is no longer being sent".to_owned())?;
    Ok((dir, beginning))
}

/// Begin `beginning` under `root`, answering the id its pieces and its send
/// name it by.
fn begin_send(root: &Path, beginning: &BeginSend) -> Result<StartedSend, String> {
    check_beginning(beginning)?;
    forget_old_sends(root);
    let send = uuid::Uuid::new_v4().simple().to_string();
    let dir = sends_root(root).join(&send);
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("could not begin the message: {error}"))?;
    let meta = serde_json::to_vec(beginning)
        .map_err(|error| format!("could not begin the message: {error}"))?;
    std::fs::write(dir.join("meta.json"), meta)
        .map_err(|error| format!("could not begin the message: {error}"))?;
    for index in 0..beginning.files.len() {
        std::fs::write(dir.join(format!("file-{index}")), b"")
            .map_err(|error| format!("could not begin the message: {error}"))?;
    }
    Ok(StartedSend { send })
}

/// Take `piece` of a begun message's file, answering the bytes the host then
/// holds of it.
fn send_piece(root: &Path, piece: &SendPiece) -> Result<PieceKept, String> {
    let (dir, beginning) = begun_send(root, &piece.send)?;
    let file = beginning
        .files
        .get(piece.file as usize)
        .ok_or_else(|| "That message carries no such file".to_string())?;
    let bytes = hex::decode(&piece.bytes).map_err(|_| "A piece is not readable".to_string())?;
    if bytes.len() > MAX_SEND_PIECE_BYTES {
        return Err(format!(
            "a piece carries at most {MAX_SEND_PIECE_BYTES} bytes"
        ));
    }
    let path = dir.join(format!("file-{}", piece.file));
    let held = std::fs::metadata(&path).map_or(0, |metadata| metadata.len());
    if piece.offset != held {
        return Err(format!(
            "the host holds {held} bytes of that file: append at offset {held}"
        ));
    }
    let end = held
        .checked_add(bytes.len() as u64)
        .filter(|end| *end <= file.size)
        .ok_or_else(|| format!("{} holds only {} bytes", file.filename, file.size))?;
    let mut open = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .map_err(|error| format!("could not take the piece: {error}"))?;
    std::io::Write::write_all(&mut open, &bytes)
        .map_err(|error| format!("could not take the piece: {error}"))?;
    Ok(PieceKept { written: end })
}

/// A begun message read for its send: where its files wait, what leaves
/// and whom it goes to, read once ([`Leaving::read`]), and what it carries.
struct Staged {
    dir: PathBuf,
    leaving: Leaving,
    files: Vec<SendFile>,
    invitation: Option<String>,
}

impl Staged {
    /// Read the message begun as `beginning`, whose files wait in `dir`.
    fn read(dir: PathBuf, beginning: BeginSend) -> Result<Self, String> {
        Ok(Self {
            dir,
            leaving: Leaving::read(beginning.message)?,
            files: beginning.files,
            invitation: beginning.calendar_ics,
        })
    }

    /// What its person is shown of it before it leaves. Every file it
    /// carries is whole by then: a file's bytes only ever grow to the size
    /// it was begun with, so what is whole when its person is asked is what
    /// leaves on their answer.
    fn shown(&self) -> Result<sending::Shown, String> {
        for (index, file) in self.files.iter().enumerate() {
            let held = std::fs::metadata(self.dir.join(format!("file-{index}")))
                .map_err(|_| format!("{} did not arrive", file.filename))?
                .len();
            if held != file.size {
                return Err(format!(
                    "{} arrived incomplete: {held} of {} bytes",
                    file.filename, file.size
                ));
            }
        }
        sending::Shown::of(&self.leaving, &self.files, self.invitation.as_deref())
    }
}

/// How long a message is taken for before another key holder may take it
/// over: long enough for a large message's files to reach its server, short
/// enough that a sender that stopped is taken over in the same minute.
const MAIL_SEND_LEASE: std::time::Duration = std::time::Duration::from_secs(10 * 60);

impl Workspaces {
    /// Send `staged` through `session` in `workspace`, where the workspace's
    /// key holders agree on which one sends a message: it is taken under a
    /// lease of its own ([`tilo_peer_state::mail_send_lease`]) and the workspace
    /// keeps what left. A message another key holder already sent is
    /// answered from what the workspace kept instead of sent again, so a
    /// takeover loses nothing and sends nothing twice; a send that failed
    /// keeps nothing and is tried again.
    async fn send_in_workspace(
        &self,
        workspace: Option<&str>,
        session: Session,
        root: &Path,
        staged: Staged,
    ) -> Result<SentMail, String> {
        let message = staged
            .leaving
            .message()
            .message_id
            .clone()
            .filter(|message| !message.is_empty());
        let Some((workspace, message)) = workspace.zip(message) else {
            return send_staged(session, root, staged).await;
        };
        if let Some(sent) = self.recorded_mail_send(Some(workspace), &staged).await {
            return Ok(sent);
        }
        let lease = tilo_peer_state::mail_send_lease(&message);
        let device = self.serving_certificate().clone();
        let now = system_time(tilo_clock::unix_now_ns());
        let claimed = self
            .author_own_workspace_state(workspace, &device, |peer| {
                peer.workspace_mut().take_lease(
                    &device,
                    &lease,
                    MAIL_SEND_LEASE,
                    tilo_peer_state::unranked(),
                    now,
                )
            })
            .await
            .map_err(|error| error.to_string())?
            .is_some();
        if !claimed {
            // Another key holder is sending it. The caller keeps the message
            // pending and asks again, and hears what left once it did.
            return Err("another device is sending that message".into());
        }
        // The merged state may hold a send made before this device took the
        // message: what left is answered, never sent a second time.
        if let Some(sent) = self.recorded_mail_send(Some(workspace), &staged).await {
            self.release_mail_send(workspace, &device, &lease, now)
                .await;
            return Ok(sent);
        }
        let outcome = send_staged(session, root, staged).await;
        if let Ok(sent) = &outcome {
            let answer = serde_json::to_value(sent).map_err(|error| error.to_string())?;
            if let Err(error) = self
                .author_own_workspace_state(workspace, &device, |peer| {
                    peer.workspace_mut().keep_done(&device, &lease, answer, now)
                })
                .await
            {
                tracing::warn!(%message, %error, "a sent message was not kept as sent");
            }
        }
        self.release_mail_send(workspace, &device, &lease, now)
            .await;
        outcome
    }

    /// Send the message begun under `sending`'s id in `workspace`: what it
    /// says, its files and its invitation, as they were begun.
    async fn send_a_message(
        &self,
        workspace: &str,
        root: &Path,
        sending: SendId,
    ) -> Result<SentMail, String> {
        let (dir, beginning) = begun_send(root, &sending.send)?;
        let staged = Staged::read(dir, beginning)?;
        self.send_in_workspace(Some(workspace), self.mail_session(), root, staged)
            .await
    }

    /// What the workspace recorded of `staged` having left, if a key holder
    /// sent it.
    async fn recorded_mail_send(
        &self,
        workspace: Option<&str>,
        staged: &Staged,
    ) -> Option<SentMail> {
        let message = staged
            .leaving
            .message()
            .message_id
            .as_deref()
            .filter(|message| !message.is_empty())?;
        let sent = self
            .state(workspace?)
            .await?
            .workspace()
            .done(&tilo_peer_state::mail_send_lease(message))?;
        match serde_json::from_value(sent.answer) {
            Ok(sent) => Some(sent),
            Err(error) => {
                tilo_failure::ignored!(
                    "what a send answered is not an answer this build reads",
                    error
                );
                None
            }
        }
    }

    /// Stop taking the message `message` for `lease`, so the next message is
    /// free to take at once rather than wait out the deadline.
    async fn release_mail_send(
        &self,
        workspace: &str,
        device: &DeviceCert,
        lease: &str,
        now: std::time::SystemTime,
    ) {
        let _ = self
            .author_own_workspace_state(workspace, device, |peer| {
                peer.workspace_mut().release_lease(device, lease, now)
            })
            .await;
    }
}

/// Send the message begun under `sending`'s id through `session`: the files
/// its pieces made, its invitation, and what it says. Answers what became of
/// its copy in the account's Sent folder.
#[cfg(test)]
async fn send_staged_with(
    session: Session,
    root: &Path,
    sending: SendId,
) -> Result<SentMail, String> {
    let (dir, beginning) = begun_send(root, &sending.send)?;
    send_staged(session, root, Staged::read(dir, beginning)?).await
}

/// Send `staged` through `session`, as it was read.
async fn send_staged(session: Session, root: &Path, staged: Staged) -> Result<SentMail, String> {
    let files = staged_files(&staged.dir, &staged.files)?;
    let outcome = send_leaving(session, root, staged.leaving, files, staged.invitation).await;
    if outcome.is_ok() {
        let _ = std::fs::remove_dir_all(&staged.dir);
    }
    outcome
}

/// Keep the message begun under `keeping`'s id in its account's Drafts
/// folder instead of sending it, and forget what was staged of it.
async fn save_staged_draft_with(
    session: Session,
    root: &Path,
    keeping: SendId,
) -> Result<(), String> {
    let (dir, beginning) = begun_send(root, &keeping.send)?;
    let files = staged_files(&dir, &beginning.files)?;
    let (account, secret) = account_with_secret(root, &beginning.message.account)?;
    let (draft, invitation) = (beginning.message, beginning.calendar_ics);
    let outcome = tokio::task::spawn_blocking(move || {
        session.save_draft(&account, &secret, &draft, &files, invitation.as_deref())
    })
    .await
    .map_err(|_| "the mail session did not finish".to_string())?;
    if outcome.is_ok() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    outcome
}

/// The files a begun message carries, each whole as its pieces made it.
fn staged_files(dir: &Path, staged: &[SendFile]) -> Result<Vec<Attachment>, String> {
    let mut files = Vec::with_capacity(staged.len());
    for (index, file) in staged.iter().enumerate() {
        let bytes = std::fs::read(dir.join(format!("file-{index}")))
            .map_err(|_| format!("{} did not arrive", file.filename))?;
        if bytes.len() as u64 != file.size {
            return Err(format!(
                "{} arrived incomplete: {} of {} bytes",
                file.filename,
                bytes.len(),
                file.size
            ));
        }
        files.push(Attachment {
            filename: file.filename.clone(),
            mime: file.mime.clone(),
            bytes,
        });
    }
    Ok(files)
}

/// The sessions a host holds with its people's mail servers
/// ([`Workspaces::mail_session`]).
pub type Session = Arc<dyn tilo_mail_session::MailSession + Send + Sync>;

/// Read one folder of one account as its server holds it, through `session`.
async fn fetch_with(
    session: Session,
    root: &Path,
    asking: FetchMail,
) -> Result<FetchedMail, String> {
    let (account, secret) = account_with_secret(root, &asking.account)?;
    tokio::task::spawn_blocking(move || session.fetch(&account, &secret, &asking))
        .await
        .map_err(|_| "the mail session did not finish".to_string())?
}

/// Read one message of one account as its server holds it.
async fn read_with(
    session: Session,
    root: &Path,
    reading: ReadMail,
) -> Result<FetchedMessage, String> {
    let (account, secret) = account_with_secret(root, &reading.account)?;
    tokio::task::spawn_blocking(move || session.read(&account, &secret, &reading))
        .await
        .map_err(|_| "the mail session did not finish".to_string())?
}

/// Ask one account's server which messages it holds that match.
async fn search_with(
    session: Session,
    root: &Path,
    searching: SearchMail,
) -> Result<FoundMail, String> {
    let (account, secret) = account_with_secret(root, &searching.account)?;
    tokio::task::spawn_blocking(move || session.search(&account, &secret, &searching))
        .await
        .map_err(|_| "the mail session did not finish".to_string())?
}

#[cfg(test)]
async fn send_with(
    session: Session,
    root: &Path,
    sending: SendMail,
    files: Vec<Attachment>,
    invitation: Option<String>,
) -> Result<SentMail, String> {
    send_leaving(session, root, Leaving::read(sending)?, files, invitation).await
}

/// Hand `leaving` to its account's server through `session`: to the
/// recipients it was read with, and to nobody else.
async fn send_leaving(
    session: Session,
    root: &Path,
    leaving: Leaving,
    files: Vec<Attachment>,
    invitation: Option<String>,
) -> Result<SentMail, String> {
    let (account, secret) = account_with_secret(root, &leaving.message().account)?;
    tokio::task::spawn_blocking(move || {
        session.send(&account, &secret, &leaving, &files, invitation.as_deref())
    })
    .await
    .map_err(|_| "the mail session did not finish".to_string())?
}

async fn mark_with(session: Session, root: &Path, marking: MarkMail) -> Result<(), String> {
    let (account, secret) = account_with_secret(root, &marking.account)?;
    tokio::task::spawn_blocking(move || session.mark(&account, &secret, &marking))
        .await
        .map_err(|_| "the mail session did not finish".to_string())?
}

/// The providers the person keeps, their models and what runs where.
pub(super) fn providers(root: &Path) -> Result<ModelProviders, String> {
    let config = provider_config(root);
    let vault = credentials(root)?;
    Ok(providers_of(&config, &vault))
}

/// `config`'s providers as the pane reads them, with `vault` saying which
/// keys are kept.
fn providers_of(
    config: &agent_provider_config::ProviderConfig,
    vault: &accounts::AccountCredentials,
) -> ModelProviders {
    ModelProviders {
        providers: config
            .profiles
            .iter()
            .map(|profile| view_of(profile, vault))
            .collect(),
        default_provider: config.default_profile.clone(),
        assignments: config
            .interaction_models
            .iter()
            .map(|(kind, chosen)| Assignment {
                kind: kind.clone(),
                provider: chosen.profile.clone(),
                model: chosen.model.clone(),
            })
            .collect(),
    }
}

/// One provider as the pane reads it.
pub(super) fn view_of(
    profile: &agent_provider_config::ProviderProfile,
    vault: &accounts::AccountCredentials,
) -> ProviderView {
    ProviderView {
        id: profile.id.clone(),
        name: profile.label.clone(),
        protocol: match profile.protocol {
            agent_provider_config::ProviderProtocol::OpenAiCompatible => {
                person_models::Protocol::OpenAiCompatible
            }
            agent_provider_config::ProviderProtocol::Anthropic => {
                person_models::Protocol::Anthropic
            }
        },
        endpoint: profile.endpoint.clone(),
        configured: vault
            .get(&key_id(&profile.id))
            .is_some_and(|secret| !secret.password.is_empty()),
        models: profile
            .models
            .iter()
            .map(|model| OfferedModel {
                id: model.id.clone(),
                label: model.label.clone(),
            })
            .collect(),
        default_model: profile.default_model.clone(),
        reasoning: profile
            .reasoning
            .as_ref()
            .filter(|kept| kept.param != agent_provider_config::ReasoningParam::Omit)
            .map(|kept| kept.effort.clone()),
    }
}

/// The person's own model configuration.
pub(super) fn provider_config(root: &Path) -> agent_provider_config::ProviderConfig {
    let store = settings_store::profile(root);
    agent_provider_config::ProviderConfig::load_in(&*store)
}

pub(super) fn save_provider_config(
    root: &Path,
    config: &agent_provider_config::ProviderConfig,
) -> Result<(), String> {
    let store = settings_store::profile(root);
    config.save_in(&*store).map_err(|error| error.to_string())
}

/// The vault id a provider's key is kept under, apart from every account's.
pub(super) fn key_id(provider: &str) -> accounts::AccountId {
    accounts::AccountId::from(format!("provider:{provider}"))
}

/// Keep the provider `adding` names, at an address this host calls
/// (`destinations`, [`kept_location`]).
pub(super) fn add_provider(
    root: &Path,
    adding: &person_models::AddProvider,
    destinations: Destinations,
) -> Result<ProviderView, String> {
    let name = adding.name.trim();
    if name.is_empty() {
        return Err("A provider needs a name".into());
    }
    let endpoint = adding.endpoint.trim();
    let location = kept_location(endpoint, destinations)?;
    let mut config = provider_config(root);
    let protocol = match adding.protocol {
        person_models::Protocol::OpenAiCompatible => {
            agent_provider_config::ProviderProtocol::OpenAiCompatible
        }
        person_models::Protocol::Anthropic => agent_provider_config::ProviderProtocol::Anthropic,
    };
    let profile = agent_provider_config::ProviderProfile {
        id: uuid::Uuid::new_v4().to_string(),
        label: name.to_owned(),
        protocol,
        endpoint: endpoint.to_owned(),
        location,
        credential: None,
        enabled: true,
        ..Default::default()
    };
    let view = view_of(&profile, &credentials(root)?);
    config.profiles.push(profile);
    save_provider_config(root, &config)?;
    Ok(view)
}

pub(super) fn set_key(root: &Path, setting: SetKey) -> Result<ProviderView, String> {
    if setting.key.trim().is_empty() {
        return Err("An API key cannot be empty".into());
    }
    let mut config = provider_config(root);
    let profile = config
        .profiles
        .iter_mut()
        .find(|profile| profile.id == setting.provider)
        .ok_or_else(|| "That provider is not kept here".to_owned())?;
    profile.credential = Some(agent_provider_config::SecretReference::provider_api_key(
        &profile.id,
    ));
    let profile = profile.clone();
    save_provider_config(root, &config)?;
    let mut vault = credentials(root)?;
    vault.put(
        &key_id(&setting.provider),
        &accounts::Secret::password(setting.key),
    );
    vault.try_save()?;
    Ok(view_of(&profile, &vault))
}

pub(super) fn remove_key(root: &Path, provider: &str) -> Result<ProviderView, String> {
    let config = provider_config(root);
    let profile = config
        .profiles
        .iter()
        .find(|profile| profile.id == provider)
        .ok_or_else(|| "That provider is not kept here".to_owned())?;
    let mut vault = credentials(root)?;
    if vault.remove(&key_id(provider)) {
        vault.try_save()?;
    }
    Ok(view_of(profile, &vault))
}

pub(super) fn remove_provider(root: &Path, provider: &str) -> Result<ModelProviders, String> {
    let mut config = provider_config(root);
    let before = config.profiles.len();
    config.profiles.retain(|profile| profile.id != provider);
    if config.profiles.len() == before {
        return Err("That provider is not kept here".into());
    }
    if config.default_profile.as_deref() == Some(provider) {
        config.default_profile = None;
    }
    config
        .interaction_models
        .retain(|_, chosen| chosen.profile != provider);
    save_provider_config(root, &config)?;
    let mut vault = credentials(root)?;
    if vault.remove(&key_id(provider)) {
        vault.try_save()?;
    }
    providers(root)
}

pub(super) fn set_assignment(
    root: &Path,
    choosing: SetAssignment,
) -> Result<ModelProviders, String> {
    let mut config = provider_config(root);
    let kind = agent_provider_config::Interaction::from_id(&choosing.kind)
        .ok_or_else(|| "That is not a kind of work".to_owned())?;
    let chosen = match (choosing.provider, choosing.model) {
        (Some(profile), Some(model)) => Some(agent_provider_config::AgentModel { profile, model }),
        (None, None) => None,
        _ => return Err("A model choice names both a provider and its model".into()),
    };
    config
        .set_interaction_model(kind, chosen)
        .map_err(str::to_owned)?;
    save_provider_config(root, &config)?;
    providers(root)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;

    fn temp_root() -> tempfile::TempDir {
        tempfile::tempdir().expect("a temporary root")
    }

    /// Every file under `dir`, for the proof that none of them carries a
    /// secret in the clear.
    fn files_under(dir: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let mut waiting = vec![dir.to_path_buf()];
        while let Some(dir) = waiting.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                // Only regular files carry a document's bytes: key storage
                // keeps other kinds of entry beside them.
                match std::fs::metadata(&path) {
                    Ok(metadata) if metadata.is_dir() => waiting.push(path),
                    Ok(metadata) if metadata.is_file() => files.push(path),
                    _ => {}
                }
            }
        }
        files
    }

    /// A provider key is kept in the person's vault, never in the settings
    /// document, and the read back says only that one is kept.
    #[test]
    fn a_provider_key_is_kept_in_the_vault_and_never_read_back() {
        let root = temp_root();
        let added = add_provider(
            root.path(),
            &person_models::AddProvider {
                name: "OpenAI".into(),
                protocol: person_models::Protocol::OpenAiCompatible,
                endpoint: "https://api.openai.com/v1".into(),
            },
            Destinations::Public,
        )
        .expect("a provider is added");
        assert!(!added.configured);

        let kept = set_key(
            root.path(),
            SetKey {
                provider: added.id.clone(),
                key: "sk-live-secret".into(),
            },
        )
        .expect("the key is kept");
        assert!(kept.configured);

        // No file the host keeps in the clear carries the key: the settings
        // documents hold a reference to it, the vault holds it sealed.
        for file in files_under(root.path()) {
            let bytes = std::fs::read(&file).expect("a file the host keeps");
            assert!(
                !bytes.windows(13).any(|window| window == b"sk-live-secret"),
                "{} carries the key in the clear",
                file.display()
            );
        }
        let listed = providers(root.path()).expect("the providers read back");
        let json = serde_json::to_string(&listed).unwrap();
        assert!(!json.contains("sk-live-secret"), "{json}");
        assert!(json.contains("\"configured\":true"), "{json}");

        remove_key(root.path(), &added.id).expect("the key is removed");
        assert!(
            !providers(root.path())
                .expect("the providers read back")
                .providers[0]
                .configured
        );
    }

    /// A provider removed takes its key with it, and its model choices stop
    /// naming it.
    #[test]
    fn removing_a_provider_takes_its_key_and_choices() {
        let root = temp_root();
        let added = add_provider(
            root.path(),
            &person_models::AddProvider {
                name: "Anthropic".into(),
                protocol: person_models::Protocol::Anthropic,
                endpoint: "https://api.anthropic.com".into(),
            },
            Destinations::Public,
        )
        .expect("a provider is added");
        set_key(
            root.path(),
            SetKey {
                provider: added.id.clone(),
                key: "sk-ant-secret".into(),
            },
        )
        .expect("the key is kept");
        let remaining = remove_provider(root.path(), &added.id).expect("the provider is removed");
        assert!(remaining.providers.is_empty());
        assert!(
            credentials(root.path())
                .expect("the vault")
                .get(&key_id(&added.id))
                .is_none()
        );
    }

    /// A provider without a name or a URL is refused before anything is
    /// kept, so a person sees why rather than an empty entry.
    #[test]
    fn an_unusable_provider_is_refused() {
        let root = temp_root();
        for (name, endpoint) in [("  ", "https://x.test"), ("Mine", "api.x.test")] {
            let refused = add_provider(
                root.path(),
                &person_models::AddProvider {
                    name: name.into(),
                    protocol: person_models::Protocol::OpenAiCompatible,
                    endpoint: endpoint.into(),
                },
                Destinations::Public,
            );
            assert!(refused.is_err(), "{name} {endpoint}");
        }
        assert!(
            providers(root.path())
                .expect("the providers")
                .providers
                .is_empty()
        );
    }

    /// A mail account's stored metadata never holds its password; a removal
    /// takes the secret with the account.
    #[test]
    fn a_mail_account_keeps_its_secret_out_of_the_store() {
        let root = temp_root();
        // The server check needs a server; keeping and removing are what the
        // store and vault do, so they are proven without one.
        let mut store = accounts::AccountStore::load_in(root.path());
        let mut account = accounts::Account::new("ada@example.test", accounts::email::TYPE_ID);
        account.config = serde_json::json!({
            "email": "ada@example.test",
            "imap_host": "imap.example.test",
            "imap_port": 993,
            "smtp_host": "smtp.example.test",
            "smtp_port": 465,
        });
        store.upsert(account);
        store.save_in(root.path()).expect("the store saves");
        let mut vault = credentials(root.path()).expect("the vault");
        vault.put(
            &accounts::AccountId::from("ada@example.test"),
            &accounts::Secret::password("hunter2"),
        );
        vault.try_save().expect("the secret is kept");

        let listed = mail_accounts(root.path()).expect("the accounts read back");
        assert_eq!(listed.accounts.len(), 1);
        assert!(listed.accounts[0].has_secret);
        let json = serde_json::to_string(&listed).unwrap();
        assert!(!json.contains("hunter2"), "{json}");

        let removed = remove_mail(root.path(), "ada@example.test").expect("the account is removed");
        assert!(removed.accounts.is_empty());
        assert!(
            credentials(root.path())
                .expect("the vault")
                .get(&accounts::AccountId::from("ada@example.test"))
                .is_none()
        );
    }
}

#[cfg(test)]
mod mail_session_tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use std::sync::Mutex;

    use super::*;

    /// A mail server that answers with the same folder and keeps what it was
    /// told, so a test sees the host asked with the account's own sign-in.
    #[derive(Default)]
    struct Fixture {
        asked: Mutex<Vec<String>>,
        carried: Mutex<Vec<(String, Vec<u8>)>>,
        invitation: Mutex<Option<String>>,
        /// Why the server's Sent folder holds no copy of what is sent next.
        unfiled: Mutex<Option<String>>,
    }

    impl tilo_mail_session::MailSession for Fixture {
        fn fetch(
            &self,
            account: &accounts::Account,
            secret: &accounts::Secret,
            asking: &FetchMail,
        ) -> Result<FetchedMail, String> {
            self.asked.lock().unwrap().push(format!(
                "fetch {} {} with {}",
                account.id, asking.folder, secret.password
            ));
            Ok(FetchedMail {
                account: asking.account.clone(),
                folder: asking.folder.clone(),
                uid_validity: 7,
                has_older: true,
                newest_uid: 42,
                messages: vec![person_accounts_message()],
                ..FetchedMail::default()
            })
        }

        fn read(
            &self,
            account: &accounts::Account,
            secret: &accounts::Secret,
            reading: &ReadMail,
        ) -> Result<FetchedMessage, String> {
            self.asked.lock().unwrap().push(format!(
                "read {} {} {} with {}",
                account.id, reading.folder, reading.uid, secret.password
            ));
            Ok(person_accounts_message())
        }

        fn search(
            &self,
            account: &accounts::Account,
            secret: &accounts::Secret,
            searching: &SearchMail,
        ) -> Result<FoundMail, String> {
            self.asked.lock().unwrap().push(format!(
                "search {} for {:?} with {}",
                account.id, searching.text, secret.password
            ));
            Ok(FoundMail {
                folders: vec![tilo_actions::remote::person_accounts::FoundFolder {
                    folder: "inbox".into(),
                    uid_validity: 7,
                    messages: vec![person_accounts_message()],
                }],
            })
        }

        fn save_draft(
            &self,
            account: &accounts::Account,
            secret: &accounts::Secret,
            draft: &SendMail,
            files: &[Attachment],
            _invitation: Option<&str>,
        ) -> Result<(), String> {
            self.asked.lock().unwrap().push(format!(
                "draft {} to {} with {} carrying {} files",
                account.id,
                draft.to,
                secret.password,
                files.len()
            ));
            Ok(())
        }

        fn send(
            &self,
            account: &accounts::Account,
            secret: &accounts::Secret,
            sending: &Leaving,
            files: &[Attachment],
            invitation: Option<&str>,
        ) -> Result<SentMail, String> {
            self.asked.lock().unwrap().push(format!(
                "send {} to {} with {} carrying {} files and invitation {:?}",
                account.id,
                sending.message().to,
                secret.password,
                files.len(),
                invitation.is_some()
            ));
            for file in files {
                self.carried
                    .lock()
                    .unwrap()
                    .push((file.filename.clone(), file.bytes.clone()));
            }
            *self.invitation.lock().unwrap() = invitation.map(str::to_owned);
            Ok(SentMail {
                unfiled: self.unfiled.lock().unwrap().clone(),
            })
        }

        fn watch(
            &self,
            _: &accounts::Account,
            _: &accounts::Secret,
            _: Option<tilo_mail_session::InboxSeen>,
            _: Arc<dyn tilo_mail_session::InboxHeard>,
        ) -> Result<tilo_mail_session::Watching, String> {
            Err("no inbox is watched in this test".into())
        }

        fn mark(
            &self,
            account: &accounts::Account,
            secret: &accounts::Secret,
            marking: &MarkMail,
        ) -> Result<(), String> {
            self.asked.lock().unwrap().push(format!(
                "mark {} {} with {}",
                account.id, marking.uid, secret.password
            ));
            Ok(())
        }
    }

    fn person_accounts_message() -> tilo_actions::remote::person_accounts::FetchedMessage {
        tilo_actions::remote::person_accounts::FetchedMessage {
            uid: 42,
            message_id: Some("42@example.test".into()),
            from: tilo_actions::remote::person_accounts::MailAddress {
                name: Some("Ada".into()),
                email: "ada@example.test".into(),
            },
            subject: "Hello".into(),
            body: "Hi".into(),
            ..tilo_actions::remote::person_accounts::FetchedMessage::default()
        }
    }

    /// A workspace that keeps an account and its sign-in answers a page's
    /// reads, sends and changes with it, and refuses an account it does not
    /// keep.
    #[test]
    fn a_page_reads_and_sends_through_the_accounts_the_person_keeps() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut account = accounts::Account::new(
            accounts::AccountId::from("ada@example.test"),
            accounts::email::TYPE_ID,
        );
        account.config = serde_json::json!({
            "email": "ada@example.test",
            "username": "ada@example.test",
            "imap_host": "imap.example.test",
            "imap_port": 993,
            "smtp_host": "smtp.example.test",
            "smtp_port": 465
        });
        let mut store = accounts::AccountStore::load_in(root);
        store.upsert(account);
        store.save_in(root).unwrap();
        let mut vault = accounts::AccountCredentials::open_in(root).unwrap();
        vault.put(
            &accounts::AccountId::from("ada@example.test"),
            &accounts::Secret::password("fixture-password"),
        );
        vault.try_save().unwrap();

        let fixture = Arc::new(Fixture::default());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let listing = mail_accounts(root).unwrap();
        assert_eq!(listing.accounts.len(), 1);
        assert!(listing.accounts[0].has_secret);

        let fetched = runtime
            .block_on(fetch_with(
                fixture.clone(),
                root,
                FetchMail {
                    account: "ada@example.test".into(),
                    folder: "inbox".into(),
                    ..FetchMail::default()
                },
            ))
            .unwrap();
        assert_eq!(fetched.messages[0].subject, "Hello");
        assert_eq!(fetched.newest_uid, 42);

        runtime
            .block_on(send_with(
                fixture.clone(),
                root,
                SendMail {
                    account: "ada@example.test".into(),
                    from_address: "ada@example.test".into(),
                    to: "bob@example.test".into(),
                    ..SendMail::default()
                },
                Vec::new(),
                None,
            ))
            .unwrap();
        runtime
            .block_on(mark_with(
                fixture.clone(),
                root,
                MarkMail {
                    account: "ada@example.test".into(),
                    folder: "inbox".into(),
                    uid: 42,
                    uid_validity: 7,
                    read: Some(true),
                    ..MarkMail::default()
                },
            ))
            .unwrap();

        let asked = fixture.asked.lock().unwrap().clone();
        assert_eq!(
            asked,
            vec![
                "fetch ada@example.test inbox with fixture-password",
                "send ada@example.test to bob@example.test with fixture-password carrying 0 files and invitation false",
                "mark ada@example.test 42 with fixture-password",
            ]
        );

        let unknown = runtime
            .block_on(fetch_with(
                fixture,
                root,
                FetchMail {
                    account: "eve@example.test".into(),
                    folder: "inbox".into(),
                    ..FetchMail::default()
                },
            ))
            .unwrap_err();
        assert!(unknown.contains("not kept here"), "{unknown}");
    }

    /// One message, what a search finds and a draft kept on its server are
    /// asked of the account's server with its person's own sign-in as well.
    #[test]
    fn a_page_reads_one_message_searches_and_keeps_a_draft_through_the_host() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        account_with_sign_in(root);
        let fixture = Arc::new(Fixture::default());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let read = runtime
            .block_on(read_with(
                fixture.clone(),
                root,
                ReadMail {
                    account: "ada@example.test".into(),
                    folder: "inbox".into(),
                    uid: 42,
                    uid_validity: 7,
                },
            ))
            .unwrap();
        assert_eq!(read.subject, "Hello");
        let found = runtime
            .block_on(search_with(
                fixture.clone(),
                root,
                SearchMail {
                    account: "ada@example.test".into(),
                    text: Some("hello".into()),
                    ..SearchMail::default()
                },
            ))
            .unwrap();
        assert_eq!(found.folders[0].messages[0].uid, 42);
        // A draft is begun as a message is, and kept instead of sent.
        let began = begin_send(
            root,
            &BeginSend {
                message: SendMail {
                    account: "ada@example.test".into(),
                    to: "bob@example.test".into(),
                    subject: "Not yet".into(),
                    ..SendMail::default()
                },
                ..BeginSend::default()
            },
        )
        .unwrap();
        let session: Session = fixture.clone();
        runtime
            .block_on(save_staged_draft_with(
                session,
                root,
                SendId {
                    send: began.send.clone(),
                },
            ))
            .unwrap();
        assert!(
            !sends_root(root).join(&began.send).exists(),
            "a kept draft leaves nothing staged"
        );

        assert_eq!(
            fixture.asked.lock().unwrap().clone(),
            vec![
                "read ada@example.test inbox 42 with fixture-password",
                "search ada@example.test for Some(\"hello\") with fixture-password",
                "draft ada@example.test to bob@example.test with fixture-password carrying 0 files",
            ]
        );
    }

    /// An account and its sign-in, as a test keeps them: the files the host
    /// reads to send for a page.
    fn account_with_sign_in(root: &Path) {
        let mut account = accounts::Account::new(
            accounts::AccountId::from("ada@example.test"),
            accounts::email::TYPE_ID,
        );
        account.config = serde_json::json!({
            "email": "ada@example.test",
            "username": "ada@example.test",
            "imap_host": "imap.example.test",
            "imap_port": 993,
            "smtp_host": "smtp.example.test",
            "smtp_port": 465
        });
        let mut store = accounts::AccountStore::load_in(root);
        store.upsert(account);
        store.save_in(root).unwrap();
        let mut vault = accounts::AccountCredentials::open_in(root).unwrap();
        vault.put(
            &accounts::AccountId::from("ada@example.test"),
            &accounts::Secret::password("fixture-password"),
        );
        vault.try_save().unwrap();
    }

    /// A page begins the message, hands its file over in pieces, and sends
    /// it: the host builds the message the pieces made, with the invitation,
    /// and forgets what it staged.
    #[test]
    fn a_page_hands_a_messages_files_over_in_pieces() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        account_with_sign_in(root);
        let fixture = Arc::new(Fixture::default());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let began = begin_send(
            root,
            &BeginSend {
                message: SendMail {
                    account: "ada@example.test".into(),
                    from_address: "ada@example.test".into(),
                    to: "bob@example.test".into(),
                    subject: "Contract".into(),
                    body: "Attached.".into(),
                    ..SendMail::default()
                },
                calendar_ics: Some("BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nEND:VCALENDAR\r\n".into()),
                files: vec![tilo_actions::remote::person_accounts::SendFile {
                    filename: "contract.pdf".into(),
                    mime: "application/pdf".into(),
                    size: 5,
                }],
            },
        )
        .unwrap();
        let kept = send_piece(
            root,
            &SendPiece {
                send: began.send.clone(),
                file: 0,
                offset: 0,
                bytes: hex::encode(b"%PDF-"),
            },
        )
        .unwrap();
        assert_eq!(kept.written, 5);

        // A piece out of order is refused, naming the offset to resume at.
        let refused = send_piece(
            root,
            &SendPiece {
                send: began.send.clone(),
                file: 0,
                offset: 3,
                bytes: hex::encode(b"x"),
            },
        )
        .unwrap_err();
        assert!(refused.contains("offset 5"), "{refused}");

        // The message leaves though its copy could not be filed, and the
        // page is told why its Sent folder holds none.
        *fixture.unfiled.lock().unwrap() = Some("Sent would not take the copy".into());
        let session: Session = fixture.clone();
        let sent = runtime
            .block_on(send_staged_with(
                session,
                root,
                SendId {
                    send: began.send.clone(),
                },
            ))
            .unwrap();
        assert_eq!(
            sent.unfiled.as_deref(),
            Some("Sent would not take the copy")
        );
        assert_eq!(
            serde_json::to_value(&sent).unwrap(),
            serde_json::json!({"unfiled": "Sent would not take the copy"})
        );
        assert_eq!(
            fixture.carried.lock().unwrap().clone(),
            vec![("contract.pdf".to_owned(), b"%PDF-".to_vec())]
        );
        assert!(fixture.invitation.lock().unwrap().is_some());
        assert!(
            !sends_root(root).join(&began.send).exists(),
            "a sent message leaves nothing staged"
        );

        // A message no longer staged is refused, never sent twice.
        let session: Session = fixture;
        let again = runtime.block_on(send_staged_with(session, root, SendId { send: began.send }));
        assert!(again.is_err(), "a sent message is gone");
    }

    /// A tester that accepts every sign-in, and counts what it was asked.
    #[derive(Default)]
    struct Accepting {
        asked: Mutex<Vec<String>>,
    }
    impl AccountTester for Accepting {
        fn test(
            &self,
            account: &accounts::Account,
            secret: &accounts::Secret,
        ) -> Result<String, String> {
            self.asked
                .lock()
                .unwrap()
                .push(format!("{} with {}", account.id, secret.password));
            Ok("signed in".into())
        }
    }

    /// An account another mail program moved in lands in the person's
    /// store with the other names it sends as and the sign-in it carried,
    /// proved before anything keeps it: the sign-in is in the vault and
    /// nowhere in the clear.
    #[test]
    fn a_migrated_account_keeps_its_identities_and_its_sign_in() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let tester = Arc::new(Accepting::default());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let kept = runtime
            .block_on(add_mail_with(
                tester.clone(),
                root,
                AddMailAccount {
                    address: "ada@example.test".into(),
                    display_name: "Ada".into(),
                    username: None,
                    incoming_host: "imap.example.test".into(),
                    incoming_port: 993,
                    outgoing_host: "smtp.example.test".into(),
                    outgoing_port: 465,
                    password: "thunderbird-secret".into(),
                    identities: vec!["Ada at work <ada@work.example.test>".into()],
                },
            ))
            .unwrap();
        assert!(kept.has_secret);
        assert_eq!(kept.address, "ada@example.test");
        assert_eq!(
            tester.asked.lock().unwrap().clone(),
            vec!["ada@example.test with thunderbird-secret".to_owned()],
            "the sign-in is proved before the account is kept"
        );

        let store = accounts::AccountStore::load_in(root);
        let account = store
            .get(&accounts::AccountId::from("ada@example.test"))
            .expect("the account is kept");
        let config = accounts::email::AccountConfig::from_value(account.config.clone()).unwrap();
        assert_eq!(
            config.identities,
            vec!["Ada at work <ada@work.example.test>".to_owned()]
        );
        assert!(!config.import_pending);

        for path in walk(root) {
            let bytes = std::fs::read(&path).unwrap();
            assert!(
                !bytes
                    .windows(19)
                    .any(|window| window == b"thunderbird-secret"),
                "{} carries the sign-in in the clear",
                path.display()
            );
        }
    }

    /// An account moved in without a saved sign-in is kept waiting for it,
    /// and no session is opened with nothing.
    #[test]
    fn an_account_moved_in_without_a_sign_in_waits_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let tester = Arc::new(Accepting::default());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let kept = runtime
            .block_on(add_mail_with(
                tester.clone(),
                root,
                AddMailAccount {
                    address: "ada@example.test".into(),
                    display_name: String::new(),
                    username: None,
                    incoming_host: "imap.example.test".into(),
                    incoming_port: 993,
                    outgoing_host: "smtp.example.test".into(),
                    outgoing_port: 465,
                    password: String::new(),
                    identities: Vec::new(),
                },
            ))
            .unwrap();
        assert!(!kept.has_secret);
        assert!(tester.asked.lock().unwrap().is_empty());
        let store = accounts::AccountStore::load_in(root);
        let account = store
            .get(&accounts::AccountId::from("ada@example.test"))
            .expect("the account is kept");
        let config = accounts::email::AccountConfig::from_value(account.config.clone()).unwrap();
        assert!(config.import_pending);
    }

    /// A sign-in is typed for the servers its account names then. One kept
    /// with no servers on it is bound when the keeper of the account asks
    /// for it, never on a call an agent makes under its own name; and once
    /// bound it is joined to no account that names another server.
    #[test]
    fn a_sign_in_is_bound_by_its_keeper_and_joined_at_no_other_server() {
        const ADA: &str = "ada@example.test";
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let id = accounts::AccountId::from(ADA);
        let names = |incoming: &str| {
            let mut settings = accounts::email::AccountConfig {
                email: ADA.into(),
                ..Default::default()
            };
            // An address it does not know names imap. and smtp. of its domain.
            settings.autofill_from_email();
            let mut settings = settings.to_value().unwrap();
            for value in settings.as_object_mut().unwrap().values_mut() {
                if value.as_str() == Some("imap.example.test") {
                    *value = incoming.into();
                }
            }
            let mut account = accounts::Account::new(id.clone(), accounts::email::TYPE_ID);
            account.config = settings;
            let mut store = accounts::AccountStore::new();
            store.upsert(account);
            store.save_in(root).unwrap();
        };
        // Kept as before the vault remembered: with no servers on it.
        names("imap.example.test");
        let mut vault = credentials(root).unwrap();
        vault.put(&id, &accounts::Secret::password("kept-for-a-test"));
        vault.try_save().unwrap();

        // An agent's call binds nothing and is refused in words.
        let refused = bound_for_an_agent(root, ADA).unwrap_err();
        assert!(refused.contains("not bound"), "{refused}");
        assert_eq!(credentials(root).unwrap().typed_for(&id), None);

        // The keeper asks: it is bound to the servers the account names now.
        bind_kept_first(root).unwrap();
        bound_for_an_agent(root, ADA).unwrap();
        let bound = credentials(root).unwrap().typed_for(&id).unwrap();
        assert!(
            bound.servers.contains("imap://imap.example.test:993"),
            "{bound:?}"
        );
        assert!(account_with_secret(root, ADA).is_ok());

        // The account comes to name another server: the sign-in does not
        // follow it there, whoever asks.
        names("imap.elsewhere.example");
        bind_kept_first(root).unwrap();
        assert_eq!(
            account_with_secret(root, ADA).unwrap_err(),
            "That account has no sign-in yet"
        );
        names("imap.example.test");
        assert!(account_with_secret(root, ADA).is_ok());
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let mut waiting = vec![dir.to_path_buf()];
        while let Some(dir) = waiting.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                match std::fs::metadata(&path) {
                    Ok(metadata) if metadata.is_dir() => waiting.push(path),
                    Ok(metadata) if metadata.is_file() => files.push(path),
                    _ => {}
                }
            }
        }
        files
    }

    /// An account kept without its sign-in reads nothing and sends nothing:
    /// the host says so instead of opening a session with no password.
    #[test]
    fn an_account_without_its_sign_in_reaches_no_server() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut account = accounts::Account::new(
            accounts::AccountId::from("ada@example.test"),
            accounts::email::TYPE_ID,
        );
        account.config = serde_json::json!({
            "email": "ada@example.test",
            "username": "ada@example.test",
            "imap_host": "imap.example.test"
        });
        let mut store = accounts::AccountStore::load_in(root);
        store.upsert(account);
        store.save_in(root).unwrap();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let refused = runtime
            .block_on(fetch_with(
                Arc::new(Fixture::default()),
                root,
                FetchMail {
                    account: "ada@example.test".into(),
                    folder: "inbox".into(),
                    ..FetchMail::default()
                },
            ))
            .unwrap_err();
        assert!(refused.contains("no sign-in"), "{refused}");
    }
}
