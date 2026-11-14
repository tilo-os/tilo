//! The person's mail accounts and model providers, kept by the host their
//! workspace runs on.
//!
//! A page has no mail connection and no vault, so the panes that add a mail
//! account or a provider key from a browser ask the host: it opens the mail
//! server's session to prove the sign-in, keeps the account and the key in
//! the workspace's own vault, and answers what it kept. A secret travels in
//! and never back — a reading says only whether one is kept.
//!
//! Both live under `people/<person>/workspace/<workspace>` in the host's
//! state: the stores are the shared account and settings formats, so a
//! device that reads them keeps reading the same files, and the vault is the
//! shared one with an owner-only key file on a host without a keyring.
//!
//! Only the person themselves asks: a device of theirs, its certificate
//! naming them. An agent's request is refused here, as the host's own
//! operations refuse one.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value};
use tilo_account_probe::{AccountTester as _, LiveAccountTester, LiveMailSession};
use tilo_actions::remote::person_accounts::{AddMailAccount, MailAccount, MailAccounts};
use tilo_actions::remote::person_accounts::{FetchMail, FetchedMail, MarkMail, SendMail};
use tilo_actions::remote::person_models::{
    Assignment, ModelProviders, OfferedModel, ProviderView, SetAssignment, SetKey,
};
use tilo_actions::remote::{
    ActionHostError, Caller, InvokeArgument, InvokeRequest, person_accounts, person_models,
};
use tilo_user_identity::DeviceCert;

use super::Workspaces;

impl Workspaces {
    /// Where this host keeps `person`'s own settings for `workspace`: their
    /// accounts and their model providers, apart from every other member's
    /// and every other workspace's.
    fn person_settings_root(&self, person: &[u8; 32], workspace: &str) -> PathBuf {
        self.inner
            .dir
            .join("person-settings")
            .join(hex::encode(person))
            .join(workspace)
    }

    /// The person `caller` asks for, on the device `certificate` certifies,
    /// in `workspace_id`: what a request that keeps a person's own secrets
    /// needs before it runs. Unknown and forbidden are the same answer.
    fn asking_person(
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        workspace_id: Option<collab_types::WorkspaceId>,
    ) -> Result<([u8; 32], String), ActionHostError> {
        let workspace = workspace_id.ok_or(ActionHostError::UnknownAction)?;
        if !caller.acting().is_person() || caller.acting().app().is_some() {
            return Err(ActionHostError::UnknownAction);
        }
        let certificate = certificate
            .filter(|certificate| {
                caller.device_public_key() == Some(&certificate.device_public_key)
                    && caller.user_public_key() == Some(&certificate.user_public_key)
            })
            .ok_or(ActionHostError::UnknownAction)?;
        Ok((certificate.user_public_key, workspace.to_string()))
    }

    /// Answer a [`person_accounts::PROVIDER`] request.
    pub(super) async fn person_accounts_invoke(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionHostError> {
        let (person, workspace) = Self::asking_person(caller, certificate, workspace_id)?;
        let root = self.person_settings_root(&person, &workspace);
        match request.target.address.action.as_str() {
            person_accounts::LIST => answer(&mail_accounts(&root)),
            person_accounts::ADD_MAIL => {
                let adding: AddMailAccount = argument(request, "an account to add")?;
                answer(&add_mail(&root, adding).await)
            }
            person_accounts::REMOVE => {
                let account = account_argument(request)?;
                answer(&remove_mail(&root, &account))
            }
            person_accounts::FETCH => {
                let asking: person_accounts::FetchMail = argument(request, "a folder to read")?;
                answer(&fetch_mail(&root, asking).await)
            }
            person_accounts::SEND => {
                let sending: person_accounts::SendMail = argument(request, "a message to send")?;
                answer(&send_mail(&root, sending).await)
            }
            person_accounts::MARK => {
                let marking: person_accounts::MarkMail =
                    argument(request, "what changed about a message")?;
                answer(&mark_mail(&root, marking).await)
            }
            _ => Err(ActionHostError::UnknownAction),
        }
    }

    /// Answer a [`person_models::PROVIDER`] request.
    pub(super) fn person_models_invoke(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        certificate: Option<&DeviceCert>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionHostError> {
        let (person, workspace) = Self::asking_person(caller, certificate, workspace_id)?;
        let root = self.person_settings_root(&person, &workspace);
        match request.target.address.action.as_str() {
            person_models::LIST => answer(&providers(&root)),
            person_models::ADD_PROVIDER => {
                let adding: person_models::AddProvider = argument(request, "a provider to add")?;
                answer(&add_provider(&root, &adding))
            }
            person_models::SET_KEY => {
                let setting: SetKey = argument(request, "a key to keep")?;
                answer(&set_key(&root, setting))
            }
            person_models::REMOVE_KEY => {
                let provider = provider_argument(request)?;
                answer(&remove_key(&root, &provider))
            }
            person_models::REMOVE_PROVIDER => {
                let provider = provider_argument(request)?;
                answer(&remove_provider(&root, &provider))
            }
            person_models::SET_ASSIGNMENT => {
                let choosing: SetAssignment = argument(request, "a model choice")?;
                answer(&set_assignment(&root, choosing))
            }
            _ => Err(ActionHostError::UnknownAction),
        }
    }
}

/// Turn one action's answer into what the caller reads, or the reason it was
/// refused.
fn answer<T: serde::Serialize>(outcome: &Result<T, String>) -> Result<Value, ActionHostError> {
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
fn argument<T: serde::de::DeserializeOwned>(
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
fn provider_argument(request: &InvokeRequest) -> Result<String, ActionHostError> {
    let value: Map<String, Value> = argument(request, "the provider")?;
    value
        .get("provider")
        .and_then(Value::as_str)
        .filter(|provider| !provider.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| invalid("the provider is named by its id"))
}

/// The vault every secret of this person's workspace is kept in.
fn credentials(root: &Path) -> Result<accounts::AccountCredentials, String> {
    accounts::AccountCredentials::open_in(root)
}

/// The mail accounts the workspace keeps, without their secrets.
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
/// workspace's vault first, then the account into the shared store, so an
/// account never shows without the sign-in it needs.
async fn add_mail(root: &Path, adding: AddMailAccount) -> Result<MailAccount, String> {
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
        // and its identities and no password: it is kept, waiting for the
        // sign-in its person completes in Settings.
        import_pending: adding.password.is_empty(),
        password: String::new(),
        identities: Vec::new(),
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
        tokio::task::spawn_blocking(move || LiveAccountTester.test(&checking, &secret))
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

/// Hand one message to its account's server.
async fn send_mail(root: &Path, sending: SendMail) -> Result<(), String> {
    send_with(Arc::new(LiveMailSession::default()), root, sending).await
}

/// Tell one message's server what changed about it.
async fn mark_mail(root: &Path, marking: MarkMail) -> Result<(), String> {
    mark_with(Arc::new(LiveMailSession::default()), root, marking).await
}

type Session = Arc<dyn tilo_account_probe::MailSession + Send + Sync>;

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

async fn send_with(session: Session, root: &Path, sending: SendMail) -> Result<(), String> {
    let (account, secret) = account_with_secret(root, &sending.account)?;
    tokio::task::spawn_blocking(move || session.send(&account, &secret, &sending))
        .await
        .map_err(|_| "the mail session did not finish".to_string())?
}

async fn mark_with(session: Session, root: &Path, marking: MarkMail) -> Result<(), String> {
    let (account, secret) = account_with_secret(root, &marking.account)?;
    tokio::task::spawn_blocking(move || session.mark(&account, &secret, &marking))
        .await
        .map_err(|_| "the mail session did not finish".to_string())?
}

/// The providers the workspace keeps, their models and what runs where.
fn providers(root: &Path) -> Result<ModelProviders, String> {
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
fn view_of(
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

/// The model configuration kept for this workspace's person.
fn provider_config(root: &Path) -> agent_provider_config::ProviderConfig {
    let store = settings_store::profile(root);
    agent_provider_config::ProviderConfig::load_in(&*store)
}

fn save_provider_config(
    root: &Path,
    config: &agent_provider_config::ProviderConfig,
) -> Result<(), String> {
    let store = settings_store::profile(root);
    config.save_in(&*store).map_err(|error| error.to_string())
}

/// The vault id a provider's key is kept under, apart from every account's.
fn key_id(provider: &str) -> accounts::AccountId {
    accounts::AccountId::from(format!("provider:{provider}"))
}

fn add_provider(root: &Path, adding: &person_models::AddProvider) -> Result<ProviderView, String> {
    let name = adding.name.trim();
    if name.is_empty() {
        return Err("A provider needs a name".into());
    }
    let endpoint = adding.endpoint.trim();
    if !(endpoint.starts_with("https://") || endpoint.starts_with("http://")) {
        return Err("A provider's endpoint is a URL".into());
    }
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
        location: agent_provider_config::EndpointLocation::Hosted,
        credential: None,
        enabled: true,
        ..Default::default()
    };
    let view = view_of(&profile, &credentials(root)?);
    config.profiles.push(profile);
    save_provider_config(root, &config)?;
    Ok(view)
}

fn set_key(root: &Path, setting: SetKey) -> Result<ProviderView, String> {
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

fn remove_key(root: &Path, provider: &str) -> Result<ProviderView, String> {
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

fn remove_provider(root: &Path, provider: &str) -> Result<ModelProviders, String> {
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

fn set_assignment(root: &Path, choosing: SetAssignment) -> Result<ModelProviders, String> {
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

    /// A provider key is kept in the workspace's vault, never in the settings
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
    }

    impl tilo_account_probe::MailSession for Fixture {
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
        ) -> Result<(), String> {
            self.asked.lock().unwrap().push(format!(
                "send {} to {} with {}",
                account.id, sending.to, secret.password
            ));
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
    fn a_page_reads_and_sends_through_the_accounts_the_workspace_keeps() {
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
                "send ada@example.test to bob@example.test with fixture-password",
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
