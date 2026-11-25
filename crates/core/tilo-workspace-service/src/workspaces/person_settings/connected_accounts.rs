//! A person's accounts at issuers an app describes, connected so the app can tell who they are
//! there.
//!
//! An app that bridges to an outside service describes that service's sign-in as data
//! ([`AccountIssuer`]). The person connects their account from the app's page
//! ([`CONNECT_ACCOUNT`]): this host signs them in as the app's public client, on the one loopback
//! redirect its client registered, so the browser comes back to this computer and nowhere else.
//! The issuer's identity endpoint confirms the account, and the person's page waits for that
//! ([`FINISH_CONNECT_ACCOUNT`]).
//!
//! What is kept is the person's alone: the grant as an item of their vault, and beside the
//! workspace, under their own folder, which app's issuer it is, the confirmed account and which
//! item holds the grant. No other member, and no app but the one that described the issuer,
//! reads any of it; that app reads only the account's id and name ([`connected`]).
//!
//! [`CONNECT_ACCOUNT`]: tilo_actions::remote::person_accounts::CONNECT_ACCOUNT
//! [`FINISH_CONNECT_ACCOUNT`]: tilo_actions::remote::person_accounts::FINISH_CONNECT_ACCOUNT

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use accounts::vault_items::{ItemId, ItemKind, NewItem, Place};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_actions::remote::person_accounts::{
    AccountIssuer, CONNECT_ACCOUNT, ConnectAccount, ConnectedAccount, FINISH_CONNECT_ACCOUNT,
    FORGET_CONNECTED_ACCOUNT, FinishConnectAccount, ForgetConnectedAccount, SignInStep,
};
use tilo_actions::remote::{ActionCallError, Caller, InvokeRequest};
use tilo_lock::MutexRecover as _;
use tilo_oauth::OAuthError;
use tilo_oauth::described::{ConfirmedAccount, DescribedIssuer};
use tokio::sync::watch;

use crate::workspaces::Workspaces;

/// How long a connect waits for the person's browser.
const CONNECT_WAIT: Duration = Duration::from_secs(10 * 60);
/// How long the person's page waits for a connect: a little past the connect's own wait.
const FINISH_WAIT: Duration = Duration::from_secs(10 * 60 + 30);
/// When a connect nobody waited for to the end is forgotten.
const FORGOTTEN_AFTER: Duration = Duration::from_secs(20 * 60);
/// The folder of a workspace's root the connected accounts are kept in, one folder per person.
const DIR: &str = "connected-accounts";
/// The most connects waiting at once on this host.
const MAX_WAITING: usize = 64;
/// The longest id an app or an issuer is kept under.
const MAX_NAME: usize = 128;

const ONLY_A_PERSON: &str = "Only a person connects their own account, from the app that asks";
const NOT_BEGUN: &str = "That connect was not begun here, or is over. Connect again";
const NOT_KEPT: &str = "The account was confirmed, but could not be kept here. Connect again";
const NOT_CONNECTED: &str = "That account is not connected here";

/// What a connect comes to: the account, or why not in words the person reads.
type Outcome = Option<Result<ConnectedAccount, String>>;

/// A connect begun and not yet waited for to the end.
struct Waiting {
    person: String,
    app: String,
    issuer: String,
    stop: Option<tilo_oauth::loopback::LoopbackCancel>,
    outcome: watch::Receiver<Outcome>,
    began: std::time::Instant,
}

static WAITING: Mutex<BTreeMap<String, Waiting>> = Mutex::new(BTreeMap::new());

/// One connected account as it is kept: nothing secret, only which vault item holds the grant.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Kept {
    app: String,
    issuer: DescribedIssuer,
    /// The public client the grant was given to, which gives it back when it is forgotten.
    client_id: String,
    account: ConfirmedAccount,
    /// The vault item holding the grant.
    grant: String,
}

impl Kept {
    fn shown(&self) -> ConnectedAccount {
        ConnectedAccount {
            app: self.app.clone(),
            issuer: self.issuer.id.clone(),
            label: self.issuer.label.clone(),
            id: self.account.id.clone(),
            name: self.account.name.clone(),
        }
    }
}

/// Whether `name` can name a folder or a file as it is: what an app id or an issuer id is.
fn plain(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn person_dir(root: &Path, person: &str) -> PathBuf {
    root.join(DIR).join(person)
}

fn file_of(root: &Path, (person, app, issuer): (&str, &str, &str)) -> PathBuf {
    person_dir(root, person)
        .join(app)
        .join(format!("{issuer}.json"))
}

/// The account kept at `path`; none where nothing is kept, or what is kept does not read.
fn read_kept(path: &Path) -> Option<Kept> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::warn!(%error, "a connected account kept here could not be read");
            return None;
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(kept) => Some(kept),
        Err(error) => {
            tracing::warn!(%error, "a connected account kept here does not read");
            None
        }
    }
}

fn write_kept(path: &Path, kept: &Kept) -> Result<(), String> {
    let text = serde_json::to_vec_pretty(kept).map_err(|error| error.to_string())?;
    let dir = path.parent().ok_or("no folder")?;
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let partial = path.with_extension("json.partial");
    std::fs::write(&partial, text).map_err(|error| error.to_string())?;
    std::fs::rename(&partial, path).map_err(|error| error.to_string())
}

/// The account `person` connected at the issuer `issuer` that `app` described, in the workspace
/// whose root is `root`: its id and name, and nothing of its grant.
fn connected(root: &Path, (person, app, issuer): (&str, &str, &str)) -> Option<ConfirmedAccount> {
    if !(plain(person) && plain(app) && plain(issuer)) {
        return None;
    }
    read_kept(&file_of(root, (person, app, issuer))).map(|kept| kept.account)
}

/// Every account `person` connected in the workspace whose root is `root`, by app and issuer.
fn connected_accounts(root: &Path, person: &str) -> Vec<ConnectedAccount> {
    if !plain(person) {
        return Vec::new();
    }
    let mut listed = Vec::new();
    let Ok(apps) = std::fs::read_dir(person_dir(root, person)) else {
        return listed;
    };
    for app in apps.flatten() {
        let Ok(issuers) = std::fs::read_dir(app.path()) else {
            continue;
        };
        for issuer in issuers.flatten() {
            let path = issuer.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
                && let Some(kept) = read_kept(&path)
            {
                listed.push(kept.shown());
            }
        }
    }
    listed.sort_by(|a, b| (&a.app, &a.issuer).cmp(&(&b.app, &b.issuer)));
    listed
}

fn described(issuer: AccountIssuer) -> DescribedIssuer {
    DescribedIssuer {
        id: issuer.id,
        label: issuer.label,
        authorization_endpoint: issuer.authorization_endpoint,
        token_endpoint: issuer.token_endpoint,
        identity_endpoint: issuer.identity_endpoint,
        revocation_endpoint: issuer.revocation_endpoint,
        id_field: issuer.id_field,
        name_field: issuer.name_field,
        scopes: issuer.scopes,
        redirect_port: issuer.redirect_port,
    }
}

/// What the person reads when a connect did not come to an account.
fn words(issuer: &DescribedIssuer, error: &OAuthError) -> String {
    let label = &issuer.label;
    match error {
        OAuthError::RedirectRefused => format!(
            "{label} did not take the address this computer's sign-in comes back to. Add {} to \
             your app's redirects",
            issuer.redirect_uri()
        ),
        OAuthError::RedirectPortsBusy => format!(
            "Another sign-in is using {} on this computer. Finish or close it, then connect again",
            issuer.redirect_uri()
        ),
        OAuthError::Denied(_) => format!("{label} was not allowed to say who you are there"),
        OAuthError::TimedOut => "The sign-in took too long. Connect again".to_owned(),
        OAuthError::Cancelled => "The sign-in was stopped".to_owned(),
        OAuthError::Network(_) => format!("{label} could not be reached. Connect again"),
        other => format!("{label} did not sign you in: {other}"),
    }
}

/// The person forgetting an account they connected: a person, from any app of theirs, never an
/// agent.
fn forgetter(caller: &Caller) -> Result<String, ActionCallError> {
    let acting = caller.acting();
    if !acting.is_person() || acting.is_agent() {
        return Err(ActionCallError::Refused {
            reason: "Only a person forgets an account they connected".to_owned(),
        });
    }
    acting
        .person_id()
        .filter(|person| plain(person))
        .map(str::to_owned)
        .ok_or(ActionCallError::UnknownAction)
}

/// The person and app a connect is asked by: a person, through the app that describes the
/// issuer. Anyone else is told the action does not exist for them.
fn asker(caller: &Caller) -> Result<(String, String), ActionCallError> {
    let acting = caller.acting();
    let refused = || ActionCallError::Refused {
        reason: ONLY_A_PERSON.to_owned(),
    };
    if !acting.is_person() || acting.is_agent() {
        return Err(refused());
    }
    let person = acting.person_id().filter(|person| plain(person));
    let app = acting
        .app()
        .map(ToString::to_string)
        .filter(|app| plain(app));
    match (person, app) {
        (Some(person), Some(app)) => Ok((person.to_owned(), app)),
        _ => Err(refused()),
    }
}

impl Workspaces {
    /// The account the person `person` (their id) connected in `workspace` at the issuer
    /// `issuer` that the app `app` described: its id and name, and nothing of its grant. What
    /// that app reads of the person invoking it, and nothing anyone else reads.
    #[must_use]
    pub fn connected_account(
        &self,
        workspace: &str,
        (person, app, issuer): (&str, &str, &str),
    ) -> Option<ConfirmedAccount> {
        connected(&self.workspace_root(workspace), (person, app, issuer))
    }

    /// Every account the person `person` (their id) connected in `workspace`, for them to see
    /// and forget.
    #[must_use]
    pub fn connected_accounts(&self, workspace: &str, person: &str) -> Vec<ConnectedAccount> {
        connected_accounts(&self.workspace_root(workspace), person)
    }

    /// Answer `request` when it is one of a person's connected accounts' actions
    /// ([`CONNECT_ACCOUNT`], [`FINISH_CONNECT_ACCOUNT`], [`FORGET_CONNECTED_ACCOUNT`]) in the
    /// workspace `workspace_id` names; `None` for any other action.
    pub(super) async fn connected_accounts_invoke(
        &self,
        workspace_id: Option<collab_types::WorkspaceId>,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Option<Result<Value, ActionCallError>> {
        if !matches!(
            request.target.address.action.as_str(),
            CONNECT_ACCOUNT | FINISH_CONNECT_ACCOUNT | FORGET_CONNECTED_ACCOUNT
        ) {
            return None;
        }
        let Some(workspace) = workspace_id else {
            return Some(Err(ActionCallError::UnknownAction));
        };
        Some(
            self.connected_invoke(&workspace.to_string(), caller, request)
                .await,
        )
    }

    async fn connected_invoke(
        &self,
        workspace: &str,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        if request.target.address.action == FORGET_CONNECTED_ACCOUNT {
            let person = forgetter(caller)?;
            let forgetting: ForgetConnectedAccount =
                super::argument(request, "the app and issuer of the account to forget")?;
            return super::answer(
                &self
                    .forget_connected(workspace, &person, &forgetting, request)
                    .await,
            );
        }
        let (person, app) = asker(caller)?;
        match request.target.address.action.as_str() {
            CONNECT_ACCOUNT => {
                let asking: ConnectAccount =
                    super::argument(request, "an issuer to connect an account at")?;
                super::answer(
                    &self
                        .connect_account(workspace, (&person, &app), asking, request)
                        .await,
                )
            }
            FINISH_CONNECT_ACCOUNT => {
                let finishing: FinishConnectAccount =
                    super::argument(request, "the connect to wait for")?;
                super::answer(&finish(&person, &app, &finishing.sign_in).await)
            }
            _ => Err(ActionCallError::UnknownAction),
        }
    }

    async fn connect_account(
        &self,
        workspace: &str,
        (person, app): (&str, &str),
        asking: ConnectAccount,
        call: &InvokeRequest,
    ) -> Result<SignInStep, String> {
        let issuer = described(asking.issuer);
        issuer.check()?;
        if !plain(&issuer.id) {
            return Err("An issuer's id is letters, digits, dots, dashes and underscores".into());
        }
        let client_id = asking.client_id.trim().to_owned();
        if client_id.is_empty() {
            return Err(format!("{} names no client to sign in as", issuer.label));
        }
        // A second connect of the same account replaces the first: its browser may never come
        // back, and the port it waits on is the one this one needs.
        stop_earlier((person, app, &issuer.id)).await;
        let mut waiting = WAITING.lock_recover();
        // A connect nobody waited for to the end is forgotten once it could no longer finish.
        waiting.retain(|_, begun| {
            tilo_clock::now().saturating_duration_since(begun.began) < FORGOTTEN_AFTER
        });
        let full = waiting.len() >= MAX_WAITING;
        drop(waiting);
        if full {
            return Err("Too many sign-ins are waiting here. Finish or close one first".into());
        }
        let authorization = tilo_oauth::client::Authorization::begin(
            &issuer.provider(),
            &issuer.registration(&client_id),
            None,
        )
        .map_err(|error| words(&issuer, &error))?;
        let open = authorization.url().to_owned();
        let id = tilo_oauth::random_token().map_err(|error| error.to_string())?;
        let (outcome, waited) = watch::channel(None);
        WAITING.lock_recover().insert(
            id.clone(),
            Waiting {
                person: person.to_owned(),
                app: app.to_owned(),
                issuer: issuer.id.clone(),
                stop: authorization.canceller(),
                outcome: waited,
                began: tilo_clock::now(),
            },
        );
        let workspaces = self.clone();
        let workspace = workspace.to_owned();
        let (person, app) = (person.to_owned(), app.to_owned());
        let call = call.clone();
        tokio::spawn(async move {
            let confirming = issuer.clone();
            let confirmed = tokio::task::spawn_blocking(move || {
                let client = tilo_oauth::client::OAuthClient::new()?;
                let tokens = authorization.complete(&client, CONNECT_WAIT)?;
                let account = confirming.confirm(&client, &tokens)?;
                Ok::<_, OAuthError>((tokens, account))
            })
            .await;
            let came = match confirmed {
                Ok(Ok((tokens, account))) => {
                    workspaces
                        .keep_connected(
                            &workspace,
                            (&person, &app),
                            &call,
                            (issuer, &client_id, account, &tokens),
                        )
                        .await
                }
                Ok(Err(error)) => Err(words(&issuer, &error)),
                Err(_) => Err(format!("{} did not sign you in to the end", issuer.label)),
            };
            outcome.send_replace(Some(came));
        });
        Ok(SignInStep {
            id,
            open,
            code: String::new(),
        })
    }

    /// Keep what a connect confirmed: the grant in `person`'s vault, in place of the one an
    /// earlier connect of the same account kept, and the account beside the workspace.
    async fn keep_connected(
        &self,
        workspace: &str,
        (person, app): (&str, &str),
        call: &InvokeRequest,
        (issuer, client_id, account, tokens): (
            DescribedIssuer,
            &str,
            ConfirmedAccount,
            &tilo_oauth::TokenSet,
        ),
    ) -> Result<ConnectedAccount, String> {
        let root = self.workspace_root(workspace);
        let path = file_of(&root, (person, app, &issuer.id));
        let earlier = read_kept(&path).and_then(|kept| ItemId::parse(&kept.grant));
        let place = Place::parse(&issuer.token_endpoint).map_err(|_| NOT_KEPT.to_owned())?;
        let grant = serde_json::to_string(tokens).map_err(|_| NOT_KEPT.to_owned())?;
        let acting = tilo_action_policy::Acting::person(person.to_owned());
        let name = account.name.clone();
        let item = self
            .vault_for(workspace, &acting)
            .serving(call)
            .changing(|items| {
                let item = items.add(NewItem::new(ItemKind::Key, vec![place], name, grant))?;
                if let Some(earlier) = &earlier {
                    items.remove(earlier);
                }
                Ok(item.id.as_str().to_owned())
            })
            .await
            .map_err(|error| {
                tracing::warn!(%error, "a connected account's grant was not kept in the vault");
                NOT_KEPT.to_owned()
            })?;
        let kept = Kept {
            app: app.to_owned(),
            issuer,
            client_id: client_id.to_owned(),
            account,
            grant: item,
        };
        write_kept(&path, &kept).map_err(|error| {
            tracing::warn!(%error, "a connected account was not kept beside its workspace");
            NOT_KEPT.to_owned()
        })?;
        self.person_settings_changed();
        Ok(kept.shown())
    }
}

impl Workspaces {
    /// Forget the account `person` connected at `forgetting`'s issuer: the grant leaves their
    /// vault, the issuer is given it back where it takes one back, and nothing of it is kept.
    /// Answers the accounts still connected.
    async fn forget_connected(
        &self,
        workspace: &str,
        person: &str,
        forgetting: &ForgetConnectedAccount,
        call: &InvokeRequest,
    ) -> Result<Vec<ConnectedAccount>, String> {
        let (app, issuer) = (forgetting.app.as_str(), forgetting.issuer.as_str());
        if !(plain(app) && plain(issuer)) {
            return Err(NOT_CONNECTED.to_owned());
        }
        let root = self.workspace_root(workspace);
        let path = file_of(&root, (person, app, issuer));
        let kept = read_kept(&path).ok_or_else(|| NOT_CONNECTED.to_owned())?;
        let acting = tilo_action_policy::Acting::person(person.to_owned());
        let grant = match (
            ItemId::parse(&kept.grant),
            Place::parse(&kept.issuer.token_endpoint),
        ) {
            (Some(id), Ok(place)) => self
                .vault_for(workspace, &acting)
                .serving(call)
                .changing(|items| {
                    let released = items.use_at(&id, &place, person).ok();
                    items.remove(&id);
                    Ok(released)
                })
                .await
                .map_err(|error| {
                    tracing::warn!(%error, "a connected account's grant stays in the vault");
                    "The account's grant could not be taken out of your vault. Try again".to_owned()
                })?,
            _ => None,
        };
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(%error, "a forgotten connected account stays kept");
                return Err("The account could not be forgotten here. Try again".to_owned());
            }
        }
        if let Some(released) = grant
            && let Ok(tokens) = serde_json::from_str::<tilo_oauth::TokenSet>(&released.secret)
        {
            give_back(kept, tokens).await;
        }
        self.person_settings_changed();
        Ok(connected_accounts(&root, person))
    }
}

/// Give a forgotten account's grant back to its issuer, where the issuer takes one back. The
/// account is forgotten here either way; an issuer that did not answer keeps a grant nobody
/// holds any more, which runs out on its own.
async fn give_back(kept: Kept, tokens: tilo_oauth::TokenSet) {
    if kept.issuer.revocation_endpoint.is_none() {
        return;
    }
    let given = tokio::task::spawn_blocking(move || {
        let client = tilo_oauth::client::OAuthClient::new()?;
        client.revoke(
            &kept.issuer.provider(),
            &kept.issuer.registration(&kept.client_id),
            &tokens,
        )
    })
    .await;
    match given {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            tracing::warn!(%error, "an issuer did not take back a forgotten account's grant");
        }
        Err(error) => {
            tracing::warn!(%error, "giving a forgotten account's grant back did not finish");
        }
    }
}

/// Stop a connect `person` began for the same account through the same app, and wait until its
/// loopback let go of the port.
async fn stop_earlier((person, app, issuer): (&str, &str, &str)) {
    let earlier: Vec<Waiting> = {
        let mut waiting = WAITING.lock_recover();
        let (theirs, others): (BTreeMap<_, _>, BTreeMap<_, _>) = std::mem::take(&mut *waiting)
            .into_iter()
            .partition(|(_, begun)| {
                begun.person == person && begun.app == app && begun.issuer == issuer
            });
        *waiting = others;
        drop(waiting);
        theirs.into_values().collect()
    };
    for mut begun in earlier {
        if let Some(stop) = begun.stop.take() {
            stop.cancel();
        }
        let _ = tokio::time::timeout(
            Duration::from_secs(5),
            begun.outcome.wait_for(Option::is_some),
        )
        .await;
    }
}

/// Wait for the connect `id` that `person` began through `app` to come back from their browser.
async fn finish(person: &str, app: &str, id: &str) -> Result<ConnectedAccount, String> {
    let mut outcome = {
        let waiting = WAITING.lock_recover();
        let begun = waiting
            .get(id)
            .filter(|begun| begun.person == person && begun.app == app)
            .ok_or_else(|| NOT_BEGUN.to_owned())?;
        let outcome = begun.outcome.clone();
        drop(waiting);
        outcome
    };
    let came = tokio::time::timeout(FINISH_WAIT, outcome.wait_for(Option::is_some))
        .await
        .map_err(|_| "The sign-in took too long. Connect again".to_owned())?
        .map_err(|_| NOT_BEGUN.to_owned())?
        .clone();
    WAITING.lock_recover().remove(id);
    came.unwrap_or_else(|| Err(NOT_BEGUN.to_owned()))
}
