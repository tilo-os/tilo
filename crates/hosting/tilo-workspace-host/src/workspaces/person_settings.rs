//! The person's mail accounts and model providers, kept by the host their
//! workspace runs on.
//!
//! A page has no mail connection and no vault, so the panes that add a mail
//! account or a provider key from a browser ask the host: it opens the mail
//! server's session to prove the sign-in, keeps the account and the key in
//! the person's own vault, and answers what it kept. A secret travels in and
//! never back — a reading says only whether one is kept.
//!
//! They are the person's, not any workspace's: one store per person under
//! `people/<person>` in the host's state, whichever of their workspaces they
//! ask from, with its own owner-only key file on a host without a keyring.
//! The stores are the shared account and settings formats, so a device that
//! reads them keeps reading the same files.
//!
//! Only the person themselves asks: a device of theirs, its certificate
//! naming them. Another member, an administrator or the workspace's owner
//! included, reaches only their own store, and an agent's request is refused
//! here, as the host's own operations refuse one.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value};
use tilo_account_probe::{AccountTester, LiveAccountTester};
use tilo_actions::remote::person_accounts::{
    AddMailAccount, BeginSend, MAX_SEND_FILE_BYTES, MAX_SEND_FILES, MAX_SEND_PIECE_BYTES,
    MAX_SEND_TOTAL_BYTES, MailAccount, MailAccounts, PieceKept, SendId, SendMail, SendPiece,
    StartedSend,
};
use tilo_actions::remote::person_accounts::{FetchMail, FetchedMail, MarkMail};
use tilo_actions::remote::person_models::{
    Assignment, ModelProviders, OfferedModel, ProviderView, SetAssignment, SetKey,
};
use tilo_actions::remote::{
    ActionHostError, Caller, InvokeArgument, InvokeRequest, person_accounts, person_models,
};
use tilo_mail_session::{Attachment, LiveMailSession};
use tilo_net::proxy::host::Destinations;
use tilo_user_identity::DeviceCert;

use super::Workspaces;
use super::person_models::kept_location;

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
    pub(super) fn accounts_listing(&self, person: &[u8; 32]) -> Result<Value, ActionHostError> {
        answer(&mail_accounts(&self.person_settings_root(person)))
    }

    /// What the person's own settings kept here changed: a window following
    /// them reads them again.
    pub(super) fn person_settings_changed(&self) {
        self.inner
            .person_settings_moved
            .send_modify(|count| *count = count.wrapping_add(1));
    }

    /// Fires whenever a person's own settings kept here change.
    pub(super) fn person_settings_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.person_settings_moved.subscribe()
    }

    /// The person `caller` asks for, on the device `certificate` certifies,
    /// from a workspace `workspace_id` names: what a request that keeps a
    /// person's own secrets needs before it runs. Unknown and forbidden are
    /// the same answer.
    pub(super) fn asking_person(
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        workspace_id: Option<collab_types::WorkspaceId>,
    ) -> Result<[u8; 32], ActionHostError> {
        workspace_id.ok_or(ActionHostError::UnknownAction)?;
        if !caller.acting().is_person() || caller.acting().app().is_some() {
            return Err(ActionHostError::UnknownAction);
        }
        let certificate = certificate
            .filter(|certificate| {
                caller.device_public_key() == Some(&certificate.device_public_key)
                    && caller.user_public_key() == Some(&certificate.user_public_key)
            })
            .ok_or(ActionHostError::UnknownAction)?;
        Ok(certificate.user_public_key)
    }

    /// Answer a [`person_accounts::PROVIDER`] request.
    pub(super) async fn person_accounts_invoke(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionHostError> {
        let person = Self::asking_person(caller, certificate, workspace_id)?;
        let root = self.person_settings_root(&person);
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
                let asking: person_accounts::FetchMail = argument(request, "a folder to read")?;
                answer(&fetch_mail(&root, asking).await)
            }
            person_accounts::SEND => {
                let sending: SendId = argument(request, "a message to send")?;
                answer(&send_staged(&root, sending).await)
            }
            person_accounts::MARK => {
                let marking: person_accounts::MarkMail =
                    argument(request, "what changed about a message")?;
                answer(&mark_mail(&root, marking).await)
            }
            person_accounts::BEGIN_SEND => {
                let beginning: BeginSend = argument(request, "a message to begin")?;
                answer(&begin_send(&root, &beginning))
            }
            person_accounts::SEND_PIECE => {
                let piece: SendPiece = argument(request, "a piece of a message's file")?;
                answer(&send_piece(&root, &piece))
            }
            _ => Err(ActionHostError::UnknownAction),
        }
    }
}

/// Turn one action's answer into what the caller reads, or the reason it was
/// refused.
pub(super) fn answer<T: serde::Serialize>(
    outcome: &Result<T, String>,
) -> Result<Value, ActionHostError> {
    match outcome {
        Ok(value) => serde_json::to_value(value).map_err(|error| ActionHostError::Refused {
            reason: error.to_string(),
        }),
        Err(reason) => Err(ActionHostError::Refused {
            reason: reason.clone(),
        }),
    }
}

fn invalid(reason: &str) -> ActionHostError {
    ActionHostError::InvalidRequest {
        reason: reason.to_owned(),
    }
}

/// The arguments of a request, as the value its action names.
pub(super) fn argument<T: serde::de::DeserializeOwned>(
    request: &InvokeRequest,
    what: &str,
) -> Result<T, ActionHostError> {
    let InvokeArgument::Json(argument) = &request.argument else {
        return Err(invalid(&format!("a request names {what}")));
    };
    serde_json::from_value(Value::Object(argument.clone()))
        .map_err(|error| invalid(&format!("{what} is not readable: {error}")))
}

/// The `account` a [`person_accounts::REMOVE`] names.
fn account_argument(request: &InvokeRequest) -> Result<String, ActionHostError> {
    let value: Map<String, Value> = argument(request, "the account to remove")?;
    value
        .get("account")
        .and_then(Value::as_str)
        .filter(|account| !account.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| invalid("removing an account names it"))
}

/// The `provider` a removing action names.
pub(super) fn provider_argument(request: &InvokeRequest) -> Result<String, ActionHostError> {
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
                    .get(&account.id)
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
        imap_tls: true,
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
        vault.put(&account.id, &accounts::Secret::password(adding.password));
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
    mail_accounts(root)
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
    let vault = credentials(root)?;
    let secret = vault
        .get(&account.id)
        .ok_or_else(|| "That account has no sign-in yet".to_owned())?;
    Ok((account, secret))
}

/// Read one folder of one account as its server holds it.
async fn fetch_mail(root: &Path, asking: FetchMail) -> Result<FetchedMail, String> {
    fetch_with(Arc::new(LiveMailSession::default()), root, asking).await
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

/// Send the message begun under `sending`'s id: the files its pieces made,
/// its invitation, and what it says.
async fn send_staged(root: &Path, sending: SendId) -> Result<(), String> {
    send_staged_with(Arc::new(LiveMailSession::default()), root, sending).await
}

/// [`send_staged`] through `session`, so a test sends to a server it stands
/// in for.
async fn send_staged_with(session: Session, root: &Path, sending: SendId) -> Result<(), String> {
    let (dir, beginning) = begun_send(root, &sending.send)?;
    let mut files = Vec::with_capacity(beginning.files.len());
    for (index, file) in beginning.files.iter().enumerate() {
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
    let outcome = send_with(
        session,
        root,
        beginning.message,
        files,
        beginning.calendar_ics,
    )
    .await;
    if outcome.is_ok() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    outcome
}

/// Tell one message's server what changed about it.
async fn mark_mail(root: &Path, marking: MarkMail) -> Result<(), String> {
    mark_with(Arc::new(LiveMailSession::default()), root, marking).await
}

type Session = Arc<dyn tilo_mail_session::MailSession + Send + Sync>;

/// [`fetch_mail`] through `session`, so a test asks a server it stands in
/// for.
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

async fn send_with(
    session: Session,
    root: &Path,
    sending: SendMail,
    files: Vec<Attachment>,
    invitation: Option<String>,
) -> Result<(), String> {
    let (account, secret) = account_with_secret(root, &sending.account)?;
    tokio::task::spawn_blocking(move || {
        session.send(&account, &secret, &sending, &files, invitation.as_deref())
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
                // Only regular files carry a document's bytes: key custody
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

        fn send(
            &self,
            account: &accounts::Account,
            secret: &accounts::Secret,
            sending: &SendMail,
            files: &[Attachment],
            invitation: Option<&str>,
        ) -> Result<(), String> {
            self.asked.lock().unwrap().push(format!(
                "send {} to {} with {} carrying {} files and invitation {:?}",
                account.id,
                sending.to,
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
            Ok(())
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

        let session: Session = fixture.clone();
        runtime
            .block_on(send_staged_with(
                session,
                root,
                SendId {
                    send: began.send.clone(),
                },
            ))
            .unwrap();
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
