//! A workspace's mail accounts, calendar accounts and model providers, kept
//! by the host the workspace runs on.
//!
//! A page has no mail connection and no vault, so the panes that add a mail
//! account or a provider key from a browser ask the host: it opens the mail
//! server's session to prove the sign-in, keeps the account and the key in
//! the workspace's vault, and answers what it kept. A secret travels in and
//! never back — a reading says only whether one is kept.
//!
//! They are the workspace's: one store per workspace
//! ([`Workspaces::workspace_root`]), for all its members, reached as the
//! workspace's policy says ([`super::workspace_stores`]). The stores are the
//! shared account and settings formats, so a device that reads them keeps
//! reading the same files.

/// The sign-ins of the accounts and providers, as entries of the workspace's vault.
pub(in crate::workspaces) mod kept_sign_ins;
pub(in crate::workspaces) mod oauth_grant;
mod sending;
/// Signing in on a provider's own page: a Nextcloud server's login flow.
mod sign_in;
pub mod turn_mail;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value};
use tilo_account_probe::AccountTester;
use tilo_actions::remote::person_accounts::{
    AddMailAccount, BeginSend, MAX_SEND_FILE_BYTES, MAX_SEND_FILES, MAX_SEND_PIECE_BYTES,
    MAX_SEND_TOTAL_BYTES, MailAccount, MailAccounts, PieceKept, SendFile, SendId, SendPiece,
    SentMail, StartedSend,
};
use tilo_actions::remote::person_accounts::{
    ArrivedAfter, FetchMail, FetchedMail, FetchedMessage, FoundMail, MarkMail, ReadMail, SearchMail,
};
// A message as text is read for sending in one place, `Leaving::read`; only
// this file's tests still name one.
use tilo_actions::remote::model_providers::{
    Assignment, ModelProviders, OfferedModel, ProviderView, SetAssignment, SetKey,
};
#[cfg(test)]
use tilo_actions::remote::person_accounts::SendMail;
use tilo_actions::remote::{
    ActionCallError, Caller, InvokeArgument, InvokeRequest, model_providers, person_accounts,
};
use tilo_mail_session::{Attachment, Leaving};
use tilo_net::proxy::host::Destinations;
use tilo_user_identity::DeviceCert;

use super::model_providers::kept_location;
use super::vault::VaultAt;
use super::workspace_stores::Store;
use super::{Workspaces, system_time};
use kept_sign_ins::{Signing, places_of, sign_in_of};

mod person_mail;
#[cfg(feature = "agents")]
use person_mail::answers_in_public;
use person_mail::{kept_mail, mail_accounts};

/// A person's calendars, kept beside their mail accounts.
#[cfg(feature = "agents")]
pub(in crate::workspaces) mod calendars;

/// A mail account signed in by a turn's telling: asked for, and carried out.
#[cfg(feature = "agents")]
pub(in crate::workspaces) mod mail_connect;

impl Workspaces {
    /// Where this host keeps what may interrupt `person`. Theirs in every
    /// workspace, and nothing a workspace keeps.
    pub(super) fn person_settings_root(&self, person: &[u8; 32]) -> PathBuf {
        self.inner.dir.join("people").join(hex::encode(person))
    }

    /// The mail accounts `workspace` keeps, as [`person_accounts::LIST`]
    /// answers them: what a window following [`person_accounts::WATCH`] is
    /// sent again whenever they change.
    pub(super) async fn accounts_listing(&self, workspace: &str) -> Result<Value, ActionCallError> {
        let held = self
            .workspace_accounts(workspace)
            .await
            .map_err(ActionCallError::refused)?;
        answer(&Ok::<_, String>(mail_accounts(
            &held,
            &self.workspace_root(workspace),
        )))
    }

    /// What a workspace's accounts or providers kept here changed: a window
    /// following them reads them again.
    pub(super) fn person_settings_changed(&self) {
        self.inner
            .person_settings_moved
            .send_modify(|count| *count = count.wrapping_add(1));
        // An inbox somebody listens for is watched with the accounts as its
        // workspace keeps them now.
        self.watch_listened_inboxes();
    }

    /// Fires whenever a workspace's accounts or providers kept here change.
    #[must_use]
    pub fn person_settings_changes(&self) -> tokio::sync::watch::Receiver<u64> {
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

    /// Keep the mail account `address` in `workspace` with a sign-in, as
    /// adding it from a window keeps one: a test stands in for the account's
    /// servers ([`Self::carry_mail_through`]).
    ///
    /// # Errors
    /// The account or its sign-in could not be kept.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn keep_mail_account_as_added(
        &self,
        workspace: &str,
        address: &str,
    ) -> Result<(), String> {
        let id = accounts::AccountId::from(address.to_owned());
        let mut account = accounts::Account::new(id, accounts::email::TYPE_ID);
        // The stand-in is every server: the account names one of its own, which its password
        // belongs to as an added account's does.
        account.config = serde_json::json!({
            "email": address,
            "username": address,
            "imap_host": "imap.stand-in.test",
            "imap_port": 993,
            "smtp_host": "smtp.stand-in.test",
            "smtp_port": 465,
        });
        let asker = self.acting_or_own(None);
        let reference = self
            .vault_for(workspace, &asker)
            .keep_sign_in(
                address,
                (accounts::vault_items::ItemKind::Login, places_of(&account)?),
                address,
                zeroize::Zeroizing::new("kept-for-a-test".to_owned()),
                None,
            )
            .await?;
        accounts::email::set_password_item(&mut account.config, Some(&reference.to_string()));
        self.keep_workspace_account((workspace, &asker), &account)
            .await?;
        self.person_settings_changed();
        Ok(())
    }

    /// What proves an account's sign-in at its server before the account is
    /// kept, for a person's own add and for a use by reference alike: one
    /// tester, whose connections go only where this server's mail may.
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

    /// Sign in to mail servers and carry their mail trusting `extra` beside the system's own
    /// authorities from now on: a test's mail server, whose certificate no authority signed,
    /// answers over real sockets. Its connections go where they did before.
    #[cfg(all(not(target_arch = "wasm32"), any(test, feature = "test-support")))]
    pub fn trust_mail_servers(&self, extra: Vec<rustls::pki_types::CertificateDer<'static>>) {
        let reach = self.inner.models.mail_reach();
        *self
            .inner
            .vault_uses
            .prover
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Arc::new(tilo_account_probe::TrustingMailTester {
                reach: reach.clone(),
                extra: extra.clone(),
            });
        self.carry_mail_through(Arc::new(
            tilo_mail_session::LiveMailSession::also_trusting(extra).reaching(reach),
        ));
    }

    /// Work out where an address's mail lives through `find` from now on, which is told the
    /// address and may ask whether a server of a name may be asked anything. Sign-ins are
    /// proved by whatever proved them before: a test stands in for the network.
    #[cfg(all(feature = "agents", any(test, feature = "test-support")))]
    pub fn find_mail_servers_through(
        &self,
        find: impl Fn(
            &str,
            &dyn Fn(&str) -> bool,
        ) -> Result<tilo_account_probe::mail_servers::MailServers, String>
        + Send
        + Sync
        + 'static,
    ) {
        struct Finding<F>(Arc<dyn AccountTester>, F);
        impl<F> AccountTester for Finding<F>
        where
            F: Fn(
                    &str,
                    &dyn Fn(&str) -> bool,
                ) -> Result<tilo_account_probe::mail_servers::MailServers, String>
                + Send
                + Sync,
        {
            fn find_mail_servers(
                &self,
                address: &str,
                allowed: &dyn Fn(&str) -> bool,
            ) -> Result<tilo_account_probe::mail_servers::MailServers, String> {
                (self.1)(address, allowed)
            }

            fn test(
                &self,
                account: &accounts::Account,
                secret: &accounts::Secret,
            ) -> Result<String, String> {
                self.0.test(account, secret)
            }
        }
        let mut prover = self
            .inner
            .vault_uses
            .prover
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let proving = Arc::clone(&prover);
        *prover = Arc::new(Finding(proving, find));
    }

    /// Keep iCloud accounts' calendars through `server_url` from now on: a
    /// test's calendar server stands in for iCloud's own.
    #[cfg(all(feature = "agents", any(test, feature = "test-support")))]
    pub fn keep_icloud_calendars_at(&self, server_url: &str) {
        *self
            .inner
            .icloud_calendars_at
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(server_url.to_owned());
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

    /// Answer a [`person_accounts::PROVIDER`] request: the workspace's mail
    /// accounts and their mail, as the workspace's policy lets the caller
    /// reach them at Mail ([`Store::MAIL`]).
    pub(super) async fn person_accounts_invoke(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        // The workspace's calendars are another resource of the same provider.
        #[cfg(feature = "agents")]
        if request.target.address.resource == calendars::RESOURCE {
            return self
                .person_calendars_invoke(workspace_id, caller, certificate, request)
                .await;
        }
        let action = request.target.address.action.as_str();
        let effect = mail_effect(action);
        let asked = (workspace_id, caller, certificate);
        // A send asks its person with the message itself, below.
        let reach = if action == person_accounts::SEND {
            self.reaching(asked, Store::MAIL, action, effect).await?
        } else {
            self.reaching_allowed(asked, Store::MAIL, action, effect, request)
                .await?
        };
        let here = reach.workspace.to_string();
        let root = self.workspace_root(&here);
        let by = reach.by();
        let signing = Signing {
            vault: self.vault_for(&here, &reach.acting),
            root: &root,
            by: &by,
        };
        let agent = reach.agent.is_some();
        // Signing in on a provider's own page: begun, stopped, and the
        // accounts as they are.
        if matches!(
            action,
            person_accounts::BEGIN_SIGN_IN | person_accounts::CANCEL_SIGN_IN
        ) {
            return sign_in::accounts_sign_in(self, reach.person, &here, request).await;
        }
        // What changes the accounts the workspace keeps is answered before
        // the rest: finding servers, adding an account, signing one in again,
        // saying what it feeds and removing it.
        if let Some(answered) =
            Box::pin(self.person_mail_action(signing, &here, &reach.acting, request)).await?
        {
            return Ok(answered);
        }
        match action {
            person_accounts::SEND => {
                let sending: SendId = argument(request, "a message to send")?;
                let (dir, beginning) = begun_for(&root, &sending.send)?;
                // The message is read once, here. Where its person is asked,
                // they are shown that reading, and that reading is what
                // leaves on their yes: nothing is read again from what was
                // begun.
                let staged = Staged::read(dir, beginning).map_err(ActionCallError::refused)?;
                if reach.answer != tilo_action_policy::Answer::Allow {
                    let shown = staged.shown().map_err(ActionCallError::refused)?;
                    // A message that left already is answered as the send
                    // it was: its person is asked nothing more.
                    if let Some(sent) = self.recorded_mail_send(Some(&here), &staged).await {
                        return answer(&Ok::<_, String>(sent));
                    }
                    caller
                        .acting()
                        .agent_id()
                        .ok_or(ActionCallError::NotPermitted)?;
                    // What leaves on their yes is what they were shown: the rows are kept
                    // apart from the message, which its agent may still change.
                    let kept = root.join(SHOWN_DIR).join(&sending.send);
                    if request.approval().is_some() {
                        let as_shown = std::fs::read(&kept)
                            .ok()
                            .and_then(|kept| {
                                serde_json::from_slice::<Vec<(String, String)>>(&kept).ok()
                            })
                            .is_some_and(|rows| rows == shown.rows());
                        let _ = std::fs::remove_file(&kept);
                        if !as_shown {
                            return Err(ActionCallError::refused(
                                "What waited is not what you were shown, so nothing was sent.",
                            ));
                        }
                    } else {
                        let rows = serde_json::to_vec(shown.rows())
                            .map_err(|error| ActionCallError::refused(error.to_string()))?;
                        std::fs::create_dir_all(root.join(SHOWN_DIR))
                            .and_then(|()| std::fs::write(&kept, rows))
                            .map_err(|error| ActionCallError::refused(error.to_string()))?;
                        return Err(ActionCallError::asks(shown.facts(caller)));
                    }
                }
                answer(
                    &self
                        .send_in_workspace(Some(&here), self.mail_session(), signing, staged)
                        .await,
                )
            }
            person_accounts::MARK => {
                let marking: MarkMail = argument(request, "what changed about a message")?;
                answer(&mark_with(self.mail_session(), signing, marking).await)
            }
            _ => {
                self.mail_read_or_draft(signing, &here, request, agent)
                    .await
            }
        }
    }

    /// Answer what reads the workspace's mail kept where `signing` says or drafts
    /// in it, for an agent (`agent`) or a person: listing the accounts,
    /// reading a folder or a message, searching, reading what arrived, and
    /// beginning a message and keeping it as a draft.
    pub(super) async fn mail_read_or_draft(
        &self,
        signing: Signing<'_>,
        workspace: &str,
        request: &InvokeRequest,
        agent: bool,
    ) -> Result<Value, ActionCallError> {
        let root = signing.root;
        match request.target.address.action.as_str() {
            person_accounts::LIST => {
                let held = signing
                    .vault
                    .accounts()
                    .await
                    .map_err(ActionCallError::refused)?;
                let mut listed = mail_accounts(&held, root);
                if agent {
                    // Whose sign-in is running is no agent's business.
                    listed.signing_in.clear();
                }
                answer(&Ok::<_, String>(listed))
            }
            person_accounts::FETCH => {
                let asking: FetchMail = argument(request, "a folder to read")?;
                answer(&fetch_with(self.mail_session(), signing, asking).await)
            }
            person_accounts::READ => {
                let reading: ReadMail = argument(request, "a message to read")?;
                answer(&read_with(self.mail_session(), signing, reading).await)
            }
            person_accounts::SEARCH => {
                let searching: SearchMail = argument(request, "what to search for")?;
                answer(&search_with(self.mail_session(), signing, searching).await)
            }
            person_accounts::ARRIVED => {
                let asking = arrived_argument(request)?;
                answer(&Ok::<_, String>(
                    self.arrived_after(workspace, asking.after),
                ))
            }
            person_accounts::BEGIN_SEND => {
                let beginning: BeginSend = argument(request, "a message to begin")?;
                answer(&begin_send(root, &beginning))
            }
            person_accounts::SEND_PIECE => {
                let piece: SendPiece = argument(request, "a piece of a message's file")?;
                begun_for(root, &piece.send)?;
                answer(&send_piece(root, &piece))
            }
            person_accounts::SAVE_DRAFT => {
                let keeping: SendId = argument(request, "a draft to keep")?;
                answer(&save_staged_draft_with(self.mail_session(), signing, keeping).await)
            }
            _ => Err(ActionCallError::UnknownAction),
        }
    }
}

/// What `action` of the workspace's mail does, as the workspace's policy
/// decides it: reading, a change kept here, or something that leaves. A
/// message begun and written piece by piece is only staged on this host:
/// nothing in the mail changes and nothing leaves until it is kept as a
/// draft or sent, which the policy weighs then.
pub(super) fn mail_effect(action: &str) -> tilo_action_policy::ActionKind {
    use tilo_action_policy::ActionKind;
    match action {
        person_accounts::LIST
        | person_accounts::WATCH
        | person_accounts::FETCH
        | person_accounts::READ
        | person_accounts::SEARCH
        | person_accounts::ARRIVED
        | person_accounts::FIND_SERVERS
        | person_accounts::BEGIN_SEND
        | person_accounts::SEND_PIECE => ActionKind::Read,
        person_accounts::SEND => ActionKind::Outside,
        _ => ActionKind::Undoable,
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

/// The arguments of a request, as the value its action names.
pub(super) fn argument<T: serde::de::DeserializeOwned>(
    request: &InvokeRequest,
    what: &str,
) -> Result<T, ActionCallError> {
    let InvokeArgument::Json(argument) = &request.argument else {
        return Err(ActionCallError::invalid_request(format!(
            "a request names {what}"
        )));
    };
    serde_json::from_value(Value::Object(argument.clone())).map_err(|error| {
        ActionCallError::invalid_request(format!(
            "{what} is not readable: {}",
            without_values(&error)
        ))
    })
}

/// `error` said without the value it was about, which may be a password:
/// serde names a field it missed between backticks, and quotes a value it
/// could not read (`invalid type: string "…", expected u16`), so its words
/// are kept up to the first quoted value.
fn without_values(error: &serde_json::Error) -> String {
    let said = error.to_string();
    match said.split_once('"') {
        Some((before, _)) => before.trim_end().to_owned(),
        None => said,
    }
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

/// The sign-in a mail account of `workspace` reads with, as the session would be given it.
#[cfg(test)]
pub(crate) async fn account_secret_for_test(
    workspaces: &Workspaces,
    workspace: &str,
    id: &str,
) -> Result<accounts::Secret, String> {
    let root = workspaces.workspace_root(workspace);
    let asker = workspaces.acting_or_own(None);
    let signing = Signing {
        vault: workspaces.vault_for(workspace, &asker),
        root: &root,
        by: "",
    };
    account_with_secret(signing, id)
        .await
        .map(|(_, secret)| secret)
}

/// The provider `account` belongs to, as a workspace's accounts reading names it.
pub(crate) fn provider_key(account: &accounts::Account) -> String {
    serde_json::to_value(person_mail::provider_of_account(account))
        .ok()
        .and_then(|provider| provider.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The `provider` a removing action names.
pub(super) fn provider_argument(request: &InvokeRequest) -> Result<String, ActionCallError> {
    let value: Map<String, Value> = argument(request, "the provider")?;
    value
        .get("provider")
        .and_then(Value::as_str)
        .filter(|provider| !provider.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| ActionCallError::invalid_request("the provider is named by its id"))
}

/// Add `adding` once `tester` saw its server accept its sign-in: the secret
/// into the workspace's vault first, then the account into the shared store, so
/// an account never shows without the sign-in it needs. Where the sign-in is an
/// entry the vault keeps already (`kept`), the account names that entry and the
/// password `adding` carries is only what is proved; nothing is kept twice. A
/// test's tester proves the store's side without a server.
pub(super) async fn add_mail_with(
    tester: Arc<dyn AccountTester>,
    (vault, root): (VaultAt<'_>, &Path),
    adding: AddMailAccount,
    kept: Option<tilo_actions::remote::person_vault::ItemRef>,
    provider: person_accounts::Provider,
    linked: Option<(accounts::Account, String)>,
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
    // Which of its services its person uses: what every later reading
    // answers and what Mail reads it by.
    let mut settings = config.to_value().map_err(|error| error.to_string())?;
    if let Value::Object(settings) = &mut settings {
        settings.insert(
            "services".into(),
            serde_json::to_value(adding.services.within(provider))
                .map_err(|error| error.to_string())?,
        );
        // The calendar account kept as its service: what reads the link back
        // when the account's services, or the account itself, go.
        if let Some((calendar, server)) = &linked {
            settings.insert("calendar_account".into(), calendar.id.as_str().into());
            settings.insert("server".into(), server.clone().into());
        }
    }
    account.config = settings;

    let before = vault
        .accounts()
        .await?
        .account(account.id.as_str())
        .and_then(|before| sign_in_of(&before.config));
    if !adding.password.is_empty() {
        let checking = account.clone();
        let secret = accounts::Secret::password(adding.password.clone());
        tokio::task::spawn_blocking(move || tester.test(&checking, &secret))
            .await
            .map_err(|_| "the mail server check did not finish".to_string())??;
        let reference = match kept {
            Some(kept) => {
                // An account kept before with a sign-in of its own lets that one go.
                if let Some(before) = &before
                    && before != &kept
                {
                    vault.forget_sign_in(account.id.as_str(), before).await?;
                }
                kept
            }
            None => {
                vault
                    .keep_sign_in(
                        account.id.as_str(),
                        (accounts::vault_items::ItemKind::Login, places_of(&account)?),
                        &config.username,
                        zeroize::Zeroizing::new(adding.password),
                        before.as_ref(),
                    )
                    .await?
            }
        };
        accounts::email::set_password_item(&mut account.config, Some(&reference.to_string()));
    } else if let Some(before) = &before {
        // Kept again without a password, it keeps the sign-in it had.
        accounts::email::set_password_item(&mut account.config, Some(&before.to_string()));
    }
    // One change keeps every account this action makes: the mail account and,
    // where its provider keeps calendars, the calendar account beside it.
    match linked {
        Some((calendar, _)) => {
            vault
                .keep_and_remove_accounts(&[account.clone(), calendar], &[])
                .await?;
        }
        None => vault.keep_account(&account).await?,
    }
    kept_mail(&vault.accounts().await?, root, account.id.as_str())
}

/// Remove `account` and its own sign-in together. A member's entry it signed in with stays
/// theirs.
async fn remove_mail(
    (vault, root): (VaultAt<'_>, &Path),
    account: &str,
) -> Result<MailAccounts, String> {
    let held = vault.accounts().await?;
    let kept = held.account(account);
    if kept.is_none() {
        return Err("That account is not kept here".into());
    }
    let sign_in = kept.as_ref().and_then(|kept| sign_in_of(&kept.config));
    // The calendars kept as its service go with it, in one change to the
    // accounts record.
    #[cfg(feature = "agents")]
    let linked = kept.as_ref().and_then(|kept| {
        calendars::linked_to(
            &held,
            kept.config.get("calendar_account").and_then(Value::as_str),
            account,
        )
    });
    #[cfg(feature = "agents")]
    let linked_sign_in = linked
        .as_ref()
        .and_then(|id| held.account(id))
        .and_then(|kept| sign_in_of(&kept.config));
    #[allow(
        unused_mut,
        reason = "a build without the calendar module removes the mail account alone"
    )]
    let mut remove = vec![account];
    #[cfg(feature = "agents")]
    if let Some(calendar) = &linked {
        remove.push(calendar.as_str());
    }
    vault.keep_and_remove_accounts(&[], &remove).await?;
    if let Some(sign_in) = &sign_in {
        vault.forget_sign_in(account, sign_in).await?;
    }
    #[cfg(feature = "agents")]
    if let Some(calendar) = &linked {
        if let Some(sign_in) = &linked_sign_in {
            vault.forget_sign_in(calendar, sign_in).await?;
        }
        calendars::forget_kept(root, calendar);
    }
    Ok(mail_accounts(&vault.accounts().await?, root))
}

/// The account `id` names, with the sign-in the workspace's vault keeps for it, the use
/// written down: what a page cannot read for itself, and what the host opens the server's
/// session with.
async fn account_with_secret(
    signing: Signing<'_>,
    id: &str,
) -> Result<(accounts::Account, accounts::Secret), String> {
    let account = signing
        .vault
        .accounts()
        .await?
        .account(id)
        .filter(|account| account.type_id.as_str() == accounts::email::TYPE_ID)
        .ok_or_else(|| "That account is not kept here".to_owned())?;
    // It reads with the entry it names, while its settings name the servers
    // that entry belongs to.
    let secret = sign_in::mail_secret(signing.vault, &account, signing.by).await?;
    Ok((account, secret))
}

/// Where a begun message waits between its pieces and its send: one
/// directory per message, holding what it says, whom it goes to and the
/// files' bytes as they arrive. It is gone once the message is sent.
fn sends_root(root: &Path) -> PathBuf {
    root.join("sends")
}

/// Where what a person is shown of a message their agent's send waits on is kept, by the
/// message's id, until their approve lets it leave.
const SHOWN_DIR: &str = "shown-sends";

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
        signing: Signing<'_>,
        staged: Staged,
    ) -> Result<SentMail, String> {
        let message = staged
            .leaving
            .message()
            .message_id
            .clone()
            .filter(|message| !message.is_empty());
        let Some((workspace, message)) = workspace.zip(message) else {
            return send_staged(session, signing, staged).await;
        };
        if let Some(sent) = self.recorded_mail_send(Some(workspace), &staged).await {
            return Ok(sent);
        }
        let lease = tilo_peer_state::mail_send_lease(&message);
        let device = self.device_cert().clone();
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
        let outcome = send_staged(session, signing, staged).await;
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
    signing: Signing<'_>,
    sending: SendId,
) -> Result<SentMail, String> {
    let (dir, beginning) = begun_send(signing.root, &sending.send)?;
    send_staged(session, signing, Staged::read(dir, beginning)?).await
}

/// Send `staged` through `session`, as it was read.
async fn send_staged(
    session: Session,
    signing: Signing<'_>,
    staged: Staged,
) -> Result<SentMail, String> {
    let files = staged_files(&staged.dir, &staged.files)?;
    let outcome = send_leaving(session, signing, staged.leaving, files, staged.invitation).await;
    if outcome.is_ok() {
        let _ = std::fs::remove_dir_all(&staged.dir);
    }
    outcome
}

/// Keep the message begun under `keeping`'s id in its account's Drafts
/// folder instead of sending it, and forget what was staged of it.
async fn save_staged_draft_with(
    session: Session,
    signing: Signing<'_>,
    keeping: SendId,
) -> Result<(), String> {
    let (dir, beginning) = begun_send(signing.root, &keeping.send)?;
    let files = staged_files(&dir, &beginning.files)?;
    let (account, secret) = account_with_secret(signing, &beginning.message.account).await?;
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
    signing: Signing<'_>,
    asking: FetchMail,
) -> Result<FetchedMail, String> {
    let (account, secret) = account_with_secret(signing, &asking.account).await?;
    tokio::task::spawn_blocking(move || session.fetch(&account, &secret, &asking))
        .await
        .map_err(|_| "the mail session did not finish".to_string())?
}

/// Read one message of one account as its server holds it.
async fn read_with(
    session: Session,
    signing: Signing<'_>,
    reading: ReadMail,
) -> Result<FetchedMessage, String> {
    let (account, secret) = account_with_secret(signing, &reading.account).await?;
    tokio::task::spawn_blocking(move || session.read(&account, &secret, &reading))
        .await
        .map_err(|_| "the mail session did not finish".to_string())?
}

/// Ask one account's server which messages it holds that match.
async fn search_with(
    session: Session,
    signing: Signing<'_>,
    searching: SearchMail,
) -> Result<FoundMail, String> {
    let (account, secret) = account_with_secret(signing, &searching.account).await?;
    tokio::task::spawn_blocking(move || session.search(&account, &secret, &searching))
        .await
        .map_err(|_| "the mail session did not finish".to_string())?
}

#[cfg(test)]
async fn send_with(
    session: Session,
    signing: Signing<'_>,
    sending: SendMail,
    files: Vec<Attachment>,
    invitation: Option<String>,
) -> Result<SentMail, String> {
    send_leaving(session, signing, Leaving::read(sending)?, files, invitation).await
}

/// Hand `leaving` to its account's server through `session`: to the
/// recipients it was read with, and to nobody else.
async fn send_leaving(
    session: Session,
    signing: Signing<'_>,
    leaving: Leaving,
    files: Vec<Attachment>,
    invitation: Option<String>,
) -> Result<SentMail, String> {
    let (account, secret) = account_with_secret(signing, &leaving.message().account).await?;
    tokio::task::spawn_blocking(move || {
        session.send(&account, &secret, &leaving, &files, invitation.as_deref())
    })
    .await
    .map_err(|_| "the mail session did not finish".to_string())?
}

async fn mark_with(
    session: Session,
    signing: Signing<'_>,
    marking: MarkMail,
) -> Result<(), String> {
    let (account, secret) = account_with_secret(signing, &marking.account).await?;
    tokio::task::spawn_blocking(move || session.mark(&account, &secret, &marking))
        .await
        .map_err(|_| "the mail session did not finish".to_string())?
}

/// The providers the person keeps, their models and what runs where.
pub(super) fn providers(root: &Path) -> ModelProviders {
    providers_of(&provider_config(root))
}

/// `config`'s providers as the pane reads them, each saying whether it names a key.
fn providers_of(config: &agent_provider_config::ProviderConfig) -> ModelProviders {
    ModelProviders {
        providers: config.profiles.iter().map(view_of).collect(),
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
        presets: agent_provider_config::presets::Preset::ALL
            .iter()
            .copied()
            .filter(|preset| !preset.signs_in_only())
            .map(|preset| model_providers::ProviderPreset {
                preset: preset.slug().to_owned(),
                name: preset.label().to_owned(),
                description: preset.description().to_owned(),
                protocol: match preset.protocol() {
                    agent_provider_config::ProviderProtocol::OpenAiCompatible => {
                        model_providers::Protocol::OpenAiCompatible
                    }
                    agent_provider_config::ProviderProtocol::Anthropic => {
                        model_providers::Protocol::Anthropic
                    }
                },
            })
            .collect(),
    }
}

/// One provider as the pane reads it.
pub(super) fn view_of(profile: &agent_provider_config::ProviderProfile) -> ProviderView {
    ProviderView {
        id: profile.id.clone(),
        name: profile.label.clone(),
        protocol: match profile.protocol {
            agent_provider_config::ProviderProtocol::OpenAiCompatible => {
                model_providers::Protocol::OpenAiCompatible
            }
            agent_provider_config::ProviderProtocol::Anthropic => {
                model_providers::Protocol::Anthropic
            }
        },
        endpoint: profile.endpoint.clone(),
        configured: key_kept(profile),
        enabled: profile.enabled,
        signs_in: profile.sign_in.is_some(),
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
        headers: profile
            .headers
            .iter()
            .map(|header| model_providers::RequestHeader {
                name: header.name.clone(),
                value: header.value.clone(),
            })
            .collect(),
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

/// The entry of the workspace's vault `profile` names as its key, or as the grant its
/// sign-in left.
pub(super) fn key_reference(
    profile: &agent_provider_config::ProviderProfile,
) -> Option<tilo_actions::remote::person_vault::ItemRef> {
    let named = match &profile.sign_in {
        Some(sign_in) => sign_in.kept.as_ref(),
        None => profile.credential.as_ref(),
    };
    named
        .and_then(agent_provider_config::SecretReference::workspace_vault_item)
        .and_then(tilo_actions::remote::person_vault::ItemRef::parse)
}

/// Whether `profile` names a key, or a grant, in the workspace's vault.
pub(super) fn key_kept(profile: &agent_provider_config::ProviderProfile) -> bool {
    key_reference(profile).is_some()
}

/// Name `reference` as `profile`'s key, or as its sign-in's grant, or with `None` name none.
fn name_key(
    profile: &mut agent_provider_config::ProviderProfile,
    reference: Option<&tilo_actions::remote::person_vault::ItemRef>,
) {
    let named = reference.map(|reference| {
        agent_provider_config::SecretReference::workspace_vault(&reference.to_string())
    });
    match &mut profile.sign_in {
        Some(sign_in) => sign_in.kept = named,
        None => profile.credential = named,
    }
}

/// The place a provider's key is sent to: its endpoint's origin.
fn key_place(
    profile: &agent_provider_config::ProviderProfile,
) -> Result<accounts::vault_items::Place, String> {
    accounts::vault_items::Place::parse(&profile.endpoint)
        .ok()
        .filter(accounts::vault_items::Place::is_web)
        .ok_or_else(|| "A provider's endpoint is a URL".to_owned())
}

/// The workspace's provider keys: entries of its vault, each named by its provider's settings
/// ([`key_reference`]) and bound to the origin of its provider's endpoint, so a key is only
/// ever sent where it was entered for. Each key read is a use the entry writes down, for `by`.
#[derive(Clone, Copy)]
pub(super) struct ProviderKeys<'a> {
    vault: VaultAt<'a>,
    by: &'a str,
}

impl<'a> ProviderKeys<'a> {
    /// The keys in `vault`, read for `by` (empty for a person or this host itself).
    pub(super) const fn of(vault: VaultAt<'a>, by: &'a str) -> Self {
        Self { vault, by }
    }

    /// The vault the keys are entries of.
    pub(super) const fn vault(&self) -> VaultAt<'a> {
        self.vault
    }

    /// The key `profile` is asked with, the use written down: none where it names none, or
    /// where the entry it names is gone or belongs to another origin than its endpoint's, which
    /// it is never sent to.
    pub(super) async fn get(
        &self,
        profile: &agent_provider_config::ProviderProfile,
    ) -> Result<Option<zeroize::Zeroizing<String>>, String> {
        let Some(reference) = key_reference(profile) else {
            return Ok(None);
        };
        let place = key_place(profile)?;
        let used = self
            .vault
            .try_sign_in(
                &reference,
                (
                    accounts::vault_items::ItemKind::Key,
                    std::slice::from_ref(&place),
                ),
                self.by,
                PROVIDER_CALL,
            )
            .await?;
        Ok(used.map(|released| released.secret))
    }

    /// Keep `key` as `profile`'s own key (or its sign-in's grant) in place of the one it kept
    /// before, and name it in `profile` for whoever keeps the settings to save.
    pub(super) async fn put(
        &self,
        profile: &mut agent_provider_config::ProviderProfile,
        key: &str,
    ) -> Result<(), String> {
        let place = key_place(profile)?;
        let reference = self
            .vault
            .keep_sign_in(
                &kept_sign_ins::provider_owner(&profile.id),
                (accounts::vault_items::ItemKind::Key, vec![place]),
                "",
                zeroize::Zeroizing::new(key.to_owned()),
                key_reference(profile).as_ref(),
            )
            .await?;
        name_key(profile, Some(&reference));
        Ok(())
    }

    /// Name the entry `reference` a member keeps as `profile`'s key: the key stays theirs, and
    /// the key the provider kept of its own before goes.
    pub(super) async fn name(
        &self,
        profile: &mut agent_provider_config::ProviderProfile,
        reference: &tilo_actions::remote::person_vault::ItemRef,
    ) -> Result<(), String> {
        if let Some(before) = key_reference(profile)
            && &before != reference
        {
            self.vault
                .forget_sign_in(&kept_sign_ins::provider_owner(&profile.id), &before)
                .await?;
        }
        name_key(profile, Some(reference));
        Ok(())
    }

    /// Let `profile`'s key go: its own entry is forgotten, a member's stays theirs, and
    /// `profile` names none.
    pub(super) async fn remove(
        &self,
        profile: &mut agent_provider_config::ProviderProfile,
    ) -> Result<(), String> {
        if let Some(before) = key_reference(profile) {
            self.vault
                .forget_sign_in(&kept_sign_ins::provider_owner(&profile.id), &before)
                .await?;
        }
        name_key(profile, None);
        Ok(())
    }
}

/// What a provider's key is used for: calling the provider.
const PROVIDER_CALL: accounts::vault_items::UsedFor<'static> = accounts::vault_items::UsedFor {
    using: "model-call",
    what: "",
    allowed: None,
};

/// Keep the provider `adding` names, at an address this host calls
/// (`destinations`, [`kept_location`]).
pub(super) fn add_provider(
    root: &Path,
    adding: &model_providers::AddProvider,
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
        model_providers::Protocol::OpenAiCompatible => {
            agent_provider_config::ProviderProtocol::OpenAiCompatible
        }
        model_providers::Protocol::Anthropic => agent_provider_config::ProviderProtocol::Anthropic,
    };
    let headers = adding
        .headers
        .iter()
        .map(|header| {
            let header = agent_provider_config::ProviderHeader {
                name: header.name.trim().to_owned(),
                value: header.value.clone(),
            };
            header.check().map(|()| header)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let profile = agent_provider_config::ProviderProfile {
        id: uuid::Uuid::new_v4().to_string(),
        label: name.to_owned(),
        protocol,
        endpoint: endpoint.to_owned(),
        location,
        credential: None,
        enabled: true,
        headers,
        ..Default::default()
    };
    let view = view_of(&profile);
    config.profiles.push(profile);
    save_provider_config(root, &config)?;
    Ok(view)
}

/// Offer `setting`'s provider to the workspace's agents, or stop offering it, and answer the
/// provider as kept. A provider stopped keeps its key and its models; the kinds of work assigned
/// to it follow the default again, so no work runs on a provider the person stopped.
pub(super) fn set_enabled(
    root: &Path,
    setting: &model_providers::SetEnabled,
) -> Result<ProviderView, String> {
    let mut config = provider_config(root);
    let profile = config
        .profiles
        .iter_mut()
        .find(|profile| profile.id == setting.provider)
        .ok_or_else(|| "That provider is not kept here".to_owned())?;
    profile.enabled = setting.enabled;
    let view = view_of(profile);
    if !setting.enabled {
        if config.default_profile.as_deref() == Some(setting.provider.as_str()) {
            config.default_profile = None;
        }
        config
            .interaction_models
            .retain(|_, chosen| chosen.profile != setting.provider);
    }
    save_provider_config(root, &config)?;
    Ok(view)
}

/// Keep `setting`'s key as its provider's: typed, its own entry of the workspace's vault; or,
/// where `kept` names a member's entry it was read from, that entry, which is not copied.
pub(super) async fn set_key(
    (keys, root): (ProviderKeys<'_>, &Path),
    setting: SetKey,
    kept: Option<&tilo_actions::remote::person_vault::ItemRef>,
) -> Result<ProviderView, String> {
    let SetKey { provider, key } = setting;
    let key = zeroize::Zeroizing::new(key);
    if key.trim().is_empty() {
        return Err("An API key cannot be empty".into());
    }
    let mut profile = provider_config(root)
        .profiles
        .into_iter()
        .find(|profile| profile.id == provider)
        .ok_or_else(|| "That provider is not kept here".to_owned())?;
    match kept {
        Some(kept) => keys.name(&mut profile, kept).await?,
        None => keys.put(&mut profile, key.trim()).await?,
    }
    save_named_key(root, &profile)
}

/// Keep the key `profile` names now in the settings under `root`, read again after the vault
/// was written so a change made to them meanwhile stays, answering the provider as kept.
pub(super) fn save_named_key(
    root: &Path,
    profile: &agent_provider_config::ProviderProfile,
) -> Result<ProviderView, String> {
    let mut config = provider_config(root);
    let named = config
        .profiles
        .iter_mut()
        .find(|kept| kept.id == profile.id)
        .ok_or_else(|| "That provider is not kept here".to_owned())?;
    named.credential.clone_from(&profile.credential);
    named.sign_in.clone_from(&profile.sign_in);
    let view = view_of(named);
    save_provider_config(root, &config)?;
    Ok(view)
}

pub(super) async fn remove_key(
    (keys, root): (ProviderKeys<'_>, &Path),
    provider: &str,
) -> Result<ProviderView, String> {
    let mut profile = provider_config(root)
        .profiles
        .into_iter()
        .find(|profile| profile.id == provider)
        .ok_or_else(|| "That provider is not kept here".to_owned())?;
    keys.remove(&mut profile).await?;
    save_named_key(root, &profile)
}

pub(super) async fn remove_provider(
    (keys, root): (ProviderKeys<'_>, &Path),
    provider: &str,
) -> Result<ModelProviders, String> {
    let mut removed = provider_config(root)
        .profiles
        .into_iter()
        .find(|profile| profile.id == provider)
        .ok_or_else(|| "That provider is not kept here".to_owned())?;
    keys.remove(&mut removed).await?;
    let mut config = provider_config(root);
    config.profiles.retain(|profile| profile.id != provider);
    if config.default_profile.as_deref() == Some(provider) {
        config.default_profile = None;
    }
    config
        .interaction_models
        .retain(|_, chosen| chosen.profile != provider);
    save_provider_config(root, &config)?;
    Ok(providers(root))
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
    Ok(providers(root))
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

    #[test]
    fn an_account_it_cannot_read_is_refused_without_its_values() {
        let request = InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: person_accounts::target(person_accounts::ADD_MAIL),
            argument: InvokeArgument::Json(
                serde_json::json!({
                    "address": "ada@example.test",
                    "incomingHost": "imap.example.test",
                    "outgoingHost": "smtp.example.test",
                    "password": "a-secret-value",
                    // A field it does not know is read past, so what it cannot
                    // read is a value of the wrong type, which serde quotes.
                    "incomingPort": "a-secret-value",
                })
                .as_object()
                .cloned()
                .unwrap(),
            ),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let Err(ActionCallError::InvalidRequest { reason }) =
            argument::<person_accounts::AddMailAccount>(&request, "an account to add")
        else {
            panic!("an account of the wrong shape is refused");
        };
        assert!(!reason.contains("a-secret-value"), "{reason}");
        assert!(reason.contains("invalid type"), "{reason}");
    }

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

    /// A workspace's vault held for one test: what the vault document keeps on a host.
    pub(super) fn held() -> &'static super::super::vault::TestStores {
        Box::leak(Box::default())
    }

    /// Every entry `vault` keeps: its password and the places it belongs to.
    pub(super) fn entries_of(
        vault: &std::sync::Mutex<accounts::vault_items::VaultItems>,
    ) -> Vec<(String, Vec<String>)> {
        vault
            .lock()
            .unwrap()
            .kept()
            .into_iter()
            .map(|(entry, _)| {
                (
                    entry.password.clone(),
                    entry.origins.iter().cloned().collect(),
                )
            })
            .collect()
    }

    /// Whether any file under `root` holds `secret` as it is.
    fn files_holding(root: &Path, secret: &str) -> Vec<PathBuf> {
        files_under(root)
            .into_iter()
            .filter(|file| {
                std::fs::read(file).is_ok_and(|bytes| {
                    bytes
                        .windows(secret.len())
                        .any(|window| window == secret.as_bytes())
                })
            })
            .collect()
    }

    /// A provider key is an entry of the workspace's vault, bound to its endpoint's origin,
    /// never in the settings document or any other file the host keeps, and the read back says
    /// only that one is kept.
    #[tokio::test]
    async fn a_provider_key_is_kept_in_the_vault_and_never_read_back() {
        let root = temp_root();
        let vault = held();
        let keys = ProviderKeys::of(VaultAt::Held(vault), "");
        let added = add_provider(
            root.path(),
            &model_providers::AddProvider {
                name: "OpenAI".into(),
                protocol: model_providers::Protocol::OpenAiCompatible,
                endpoint: "https://api.openai.com/v1".into(),
                headers: Vec::new(),
            },
            Destinations::Public,
        )
        .expect("a provider is added");
        assert!(!added.configured);

        let kept = set_key(
            (keys, root.path()),
            SetKey {
                provider: added.id.clone(),
                key: "sk-live-secret".into(),
            },
            None,
        )
        .await
        .expect("the key is kept");
        assert!(kept.configured);
        assert_eq!(
            entries_of(vault),
            [(
                "sk-live-secret".to_owned(),
                vec!["https://api.openai.com".to_owned()]
            )]
        );
        assert_eq!(
            files_holding(root.path(), "sk-live-secret"),
            Vec::<PathBuf>::new(),
            "the settings name the key; no file the host keeps holds it"
        );
        let listed = providers(root.path());
        let json = serde_json::to_string(&listed).unwrap();
        assert!(!json.contains("sk-live-secret"), "{json}");
        assert!(json.contains("\"configured\":true"), "{json}");

        remove_key((keys, root.path()), &added.id)
            .await
            .expect("the key is removed");
        assert!(!providers(root.path()).providers[0].configured);
        assert!(entries_of(vault).is_empty(), "the key's entry went with it");
    }

    /// A provider removed takes its key with it, and its model choices stop
    /// naming it.
    #[tokio::test]
    async fn removing_a_provider_takes_its_key_and_choices() {
        let root = temp_root();
        let vault = held();
        let keys = ProviderKeys::of(VaultAt::Held(vault), "");
        let added = add_provider(
            root.path(),
            &model_providers::AddProvider {
                name: "Anthropic".into(),
                protocol: model_providers::Protocol::Anthropic,
                endpoint: "https://api.anthropic.com".into(),
                headers: Vec::new(),
            },
            Destinations::Public,
        )
        .expect("a provider is added");
        set_key(
            (keys, root.path()),
            SetKey {
                provider: added.id.clone(),
                key: "sk-ant-secret".into(),
            },
            None,
        )
        .await
        .expect("the key is kept");
        assert_eq!(entries_of(vault).len(), 1);
        let remaining = remove_provider((keys, root.path()), &added.id)
            .await
            .expect("the provider is removed");
        assert!(remaining.providers.is_empty());
        assert!(entries_of(vault).is_empty());
    }

    /// A key is kept for its provider's endpoint origin, as a Tilo computer
    /// keeps it: pointed elsewhere, the provider has no key until one is
    /// entered for the new address, and the old one is never sent there.
    #[tokio::test]
    async fn a_key_does_not_follow_an_edited_endpoint() {
        let root = temp_root();
        let keys = ProviderKeys::of(VaultAt::Held(held()), "");
        let added = add_provider(
            root.path(),
            &model_providers::AddProvider {
                name: "Mine".into(),
                protocol: model_providers::Protocol::OpenAiCompatible,
                endpoint: "https://models.example.com/v1".into(),
                headers: Vec::new(),
            },
            Destinations::Public,
        )
        .expect("a provider is added");
        let kept = set_key(
            (keys, root.path()),
            SetKey {
                provider: added.id,
                key: "sk-for-example".into(),
            },
            None,
        )
        .await
        .expect("the key is kept");
        assert!(kept.configured);
        let at_home = provider_config(root.path()).profiles[0].clone();
        assert_eq!(
            keys.get(&at_home)
                .await
                .unwrap()
                .as_deref()
                .map(String::as_str),
            Some("sk-for-example")
        );

        let mut config = provider_config(root.path());
        config.profiles[0].endpoint = "https://elsewhere.example.net/v1".into();
        save_provider_config(root.path(), &config).unwrap();
        let moved = &provider_config(root.path()).profiles[0];
        assert_eq!(keys.get(moved).await.unwrap(), None);
    }

    /// A provider without a name or a URL is refused before anything is
    /// kept, so a person sees why rather than an empty entry.
    #[test]
    fn an_unusable_provider_is_refused() {
        let root = temp_root();
        for (name, endpoint) in [("  ", "https://x.test"), ("Mine", "api.x.test")] {
            let refused = add_provider(
                root.path(),
                &model_providers::AddProvider {
                    name: name.into(),
                    protocol: model_providers::Protocol::OpenAiCompatible,
                    endpoint: endpoint.into(),
                    headers: Vec::new(),
                },
                Destinations::Public,
            );
            assert!(refused.is_err(), "{name} {endpoint}");
        }
        assert!(providers(root.path()).providers.is_empty());
    }

    /// A mail account's stored settings never hold its password, only the entry of the
    /// workspace's vault that does. A removal takes the account's own entry with it, and leaves
    /// a member's entry another account signed in with.
    #[tokio::test]
    async fn a_mail_account_keeps_its_secret_out_of_the_store() {
        let root = temp_root();
        let vault = held();
        let at = VaultAt::Held(vault);
        // The server check needs a server; keeping and removing are what the
        // store and vault do, so they are proven without one.
        for (address, own) in [("ada@example.test", true), ("bo@example.test", false)] {
            let mut account = accounts::Account::new(address, accounts::email::TYPE_ID);
            account.config = serde_json::json!({
                "email": address,
                "imap_host": "imap.example.test",
                "imap_port": 993,
                "smtp_host": "smtp.example.test",
                "smtp_port": 465,
            });
            let places = kept_sign_ins::places_of(&account).unwrap();
            let password = format!("{}-hunter2", &address[..2]);
            let reference = if own {
                at.keep_sign_in(
                    address,
                    (accounts::vault_items::ItemKind::Login, places),
                    address,
                    zeroize::Zeroizing::new(password),
                    None,
                )
                .await
                .unwrap()
            } else {
                let added = vault
                    .lock()
                    .unwrap()
                    .add(accounts::vault_items::NewItem::new(
                        accounts::vault_items::ItemKind::Login,
                        places,
                        address.to_owned(),
                        password,
                    ))
                    .unwrap();
                tilo_actions::remote::person_vault::ItemRef::from_id(added.id.as_str()).unwrap()
            };
            accounts::email::set_password_item(&mut account.config, Some(&reference.to_string()));
            vault.keep_account(&account);
        }

        let listed = mail_accounts(&at.accounts().await.unwrap(), root.path());
        assert_eq!(listed.accounts.len(), 2);
        assert!(listed.accounts.iter().all(|account| account.has_secret));
        let json = serde_json::to_string(&listed).unwrap();
        assert!(!json.contains("hunter2"), "{json}");
        assert_eq!(files_holding(root.path(), "hunter2"), Vec::<PathBuf>::new());

        let removed = remove_mail((at, root.path()), "ada@example.test")
            .await
            .expect("the account is removed");
        assert_eq!(removed.accounts.len(), 1);
        remove_mail((at, root.path()), "bo@example.test")
            .await
            .expect("the other is removed");
        assert_eq!(
            entries_of(vault)
                .into_iter()
                .map(|(password, _)| password)
                .collect::<Vec<_>>(),
            ["bo-hunter2"],
            "the account's own sign-in went with it, and the member's entry stays theirs"
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
        let vault = super::tests::held();
        let signing = Signing {
            vault: VaultAt::Held(vault),
            root,
            by: "",
        };
        let fixture = Arc::new(Fixture::default());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(account_with_sign_in(signing));
        let listing = mail_accounts(&vault.accounts_now(), root);
        assert_eq!(listing.accounts.len(), 1);
        assert!(listing.accounts[0].has_secret);

        let fetched = runtime
            .block_on(fetch_with(
                fixture.clone(),
                signing,
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
                signing,
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
                signing,
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
                signing,
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
        let signing = Signing {
            vault: VaultAt::Held(super::tests::held()),
            root,
            by: "",
        };
        let fixture = Arc::new(Fixture::default());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(account_with_sign_in(signing));

        let read = runtime
            .block_on(read_with(
                fixture.clone(),
                signing,
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
                signing,
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
                signing,
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

    /// An account and its sign-in, as a test keeps them: the account in the
    /// workspace's accounts, naming its entry of the workspace's vault.
    async fn account_with_sign_in(signing: Signing<'_>) {
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
        let reference = signing
            .vault
            .keep_sign_in(
                "ada@example.test",
                (
                    accounts::vault_items::ItemKind::Login,
                    places_of(&account).unwrap(),
                ),
                "ada@example.test",
                zeroize::Zeroizing::new("fixture-password".to_owned()),
                None,
            )
            .await
            .unwrap();
        accounts::email::set_password_item(&mut account.config, Some(&reference.to_string()));
        signing.vault.keep_account(&account).await.unwrap();
    }

    /// A page begins the message, hands its file over in pieces, and sends
    /// it: the host builds the message the pieces made, with the invitation,
    /// and forgets what it staged.
    #[test]
    fn a_page_hands_a_messages_files_over_in_pieces() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let signing = Signing {
            vault: VaultAt::Held(super::tests::held()),
            root,
            by: "",
        };
        let fixture = Arc::new(Fixture::default());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(account_with_sign_in(signing));

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
                signing,
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
        let again = runtime.block_on(send_staged_with(
            session,
            signing,
            SendId { send: began.send },
        ));
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
        let vault = super::tests::held();
        let tester = Arc::new(Accepting::default());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let kept = runtime
            .block_on(add_mail_with(
                tester.clone(),
                (VaultAt::Held(vault), root),
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
                    services: person_accounts::Services::default(),
                },
                None,
                person_accounts::Provider::Other,
                None,
            ))
            .unwrap();
        assert!(kept.has_secret);
        assert_eq!(kept.address, "ada@example.test");
        assert_eq!(
            tester.asked.lock().unwrap().clone(),
            vec!["ada@example.test with thunderbird-secret".to_owned()],
            "the sign-in is proved before the account is kept"
        );

        let account = vault
            .account("ada@example.test")
            .expect("the account is kept");
        let config = accounts::email::AccountConfig::from_value(account.config).unwrap();
        assert_eq!(
            config.identities,
            vec!["Ada at work <ada@work.example.test>".to_owned()]
        );
        assert!(!config.import_pending);
        assert_eq!(
            super::tests::entries_of(vault),
            [(
                "thunderbird-secret".to_owned(),
                vec![
                    "imap://imap.example.test:993".to_owned(),
                    "smtp://smtp.example.test:465".to_owned()
                ]
            )],
            "the sign-in is the account's own entry of the workspace's vault, at its servers"
        );

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
        let vault = super::tests::held();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let kept = runtime
            .block_on(add_mail_with(
                tester.clone(),
                (VaultAt::Held(vault), root),
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
                    services: person_accounts::Services::default(),
                },
                None,
                person_accounts::Provider::Other,
                None,
            ))
            .unwrap();
        assert!(!kept.has_secret);
        assert!(tester.asked.lock().unwrap().is_empty());
        let account = vault
            .account("ada@example.test")
            .expect("the account is kept");
        let config = accounts::email::AccountConfig::from_value(account.config).unwrap();
        assert!(config.import_pending);
    }

    /// A sign-in belongs to the servers its account names when it is kept. An
    /// account that comes to name another server signs in nowhere with it,
    /// whoever asks, until it names its own servers again.
    #[test]
    fn a_sign_in_is_used_at_its_own_servers_alone() {
        const ADA: &str = "ada@example.test";
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let stores = super::tests::held();
        let signing = Signing {
            vault: VaultAt::Held(stores),
            root,
            by: "",
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(account_with_sign_in(signing));
        let names = |incoming: &str| {
            let mut account = stores.account(ADA).unwrap();
            account.config["imap_host"] = incoming.into();
            stores.keep_account(&account);
        };
        let (_, secret) = runtime.block_on(account_with_secret(signing, ADA)).unwrap();
        assert_eq!(secret.password, "fixture-password");

        names("imap.elsewhere.example");
        let refused = runtime
            .block_on(account_with_secret(signing, ADA))
            .unwrap_err();
        assert!(refused.contains("other servers"), "{refused}");
        let agent = Signing {
            by: "scout",
            ..signing
        };
        assert!(runtime.block_on(account_with_secret(agent, ADA)).is_err());

        names("imap.example.test");
        assert!(runtime.block_on(account_with_secret(signing, ADA)).is_ok());
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
        let stores = super::tests::held();
        let signing = Signing {
            vault: VaultAt::Held(stores),
            root,
            by: "",
        };
        let mut account = accounts::Account::new(
            accounts::AccountId::from("ada@example.test"),
            accounts::email::TYPE_ID,
        );
        account.config = serde_json::json!({
            "email": "ada@example.test",
            "username": "ada@example.test",
            "imap_host": "imap.example.test"
        });
        stores.keep_account(&account);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let refused = runtime
            .block_on(fetch_with(
                Arc::new(Fixture::default()),
                signing,
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
