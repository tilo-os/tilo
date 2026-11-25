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
//! What is kept is the person's alone, in their own workspace, so it reaches every device of
//! theirs: the grant as an item of its vault, and in the record of their connected accounts
//! which app's issuer it is, the confirmed account and which item holds the grant. No other
//! member, and no app but the one that described the issuer, reads any of it; that app reads
//! only the account's id and name ([`Workspaces::connected_account`]).
//!
//! [`CONNECT_ACCOUNT`]: tilo_actions::remote::person_accounts::CONNECT_ACCOUNT
//! [`FINISH_CONNECT_ACCOUNT`]: tilo_actions::remote::person_accounts::FINISH_CONNECT_ACCOUNT

use std::collections::BTreeMap;
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

use tilo_server_protocol::servers::person_workspace_id;

use crate::workspaces::Workspaces;
use crate::workspaces::person_workspace;
use crate::workspaces::person_workspace::connected::{
    CONNECTED, Connected, ConnectedAccounts, entry_id,
};

/// How long a connect waits for the person's browser.
const CONNECT_WAIT: Duration = Duration::from_secs(10 * 60);
/// How long the person's page waits for a connect: a little past the connect's own wait.
const FINISH_WAIT: Duration = Duration::from_secs(10 * 60 + 30);
/// When a connect nobody waited for to the end is forgotten.
const FORGOTTEN_AFTER: Duration = Duration::from_secs(20 * 60);
/// The most connects waiting at once on this host.
const MAX_WAITING: usize = 64;
/// The longest id an app or an issuer is kept under.
const MAX_NAME: usize = 128;

const ONLY_A_PERSON: &str = "Only a person connects their own account, from the app that asks";
const NOT_BEGUN: &str = "That connect was not begun here, or is over. Connect again";
const NOT_KEPT: &str = "The account was confirmed, but could not be kept here. Connect again";
const NOT_CONNECTED: &str = "That account is not connected";
const NOT_THEIRS: &str =
    "Your connected accounts are on your own devices, and this is not one of them";
const NOT_YET: &str =
    "This device has not received your connected accounts yet. Try again in a moment";

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

/// What gives a connected account's grant back when it is forgotten: the issuer as the app
/// described it and the public client the grant was given to. Kept as the record's `described`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Described {
    issuer: DescribedIssuer,
    client_id: String,
}

/// The issuer and client `kept` was connected with; none when what is kept of them does not
/// read, which only a record this host did not write has.
fn described_of(kept: &Connected) -> Option<Described> {
    match serde_json::from_str::<Described>(&kept.described) {
        Ok(described) => Some(described),
        Err(error) => {
            tracing::warn!(%error, "a connected account's issuer does not read");
            None
        }
    }
}

fn shown(kept: &Connected) -> ConnectedAccount {
    let label =
        described_of(kept).map_or_else(|| kept.issuer.clone(), |described| described.issuer.label);
    ConnectedAccount {
        app: kept.app.clone(),
        issuer: kept.issuer.clone(),
        label,
        id: kept.account.clone(),
        name: kept.name.clone(),
    }
}

/// Whether `name` is what an app id or an issuer id is: letters, digits, dots, dashes and
/// underscores.
fn plain(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
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
    /// The person's own workspace their connected accounts are kept in, when `person` (their
    /// id) is this device's person: a device of anybody else's keeps none of it.
    fn connected_workspace(&self, person: &str) -> Option<String> {
        let own = self.device_cert().user_public_key;
        (hex::encode(own) == person).then(|| person_workspace_id(&own))
    }

    /// The record of `person`'s connected accounts, as this device reads it: none where this
    /// device is not theirs, has not received their own workspace yet, or it does not read.
    async fn connected_record(&self, person: &str) -> Option<ConnectedAccounts> {
        let workspace = self.connected_workspace(person)?;
        if !person_workspace::holds_person_workspace(self).await {
            return None;
        }
        match person_workspace::read_on::<ConnectedAccounts>(self, &workspace, CONNECTED).await {
            Ok(record) => Some(record),
            Err(error) => {
                tracing::warn!(%error, "the person's connected accounts are not read");
                None
            }
        }
    }

    /// The person's own workspace, for a change to their connected accounts on a device of
    /// theirs that has received it; the words they read otherwise.
    async fn changing_connected(&self, person: &str) -> Result<String, String> {
        let workspace = self
            .connected_workspace(person)
            .ok_or_else(|| NOT_THEIRS.to_owned())?;
        if !person_workspace::holds_person_workspace(self).await {
            return Err(NOT_YET.to_owned());
        }
        Ok(workspace)
    }

    /// The account the person `person` (their id) connected at the issuer `issuer` that the
    /// app `app` described: its id and name, and nothing of its grant. What that app reads of
    /// the person invoking it, the same on every device of theirs, and nothing anyone else
    /// reads.
    pub async fn connected_account(
        &self,
        (person, app, issuer): (&str, &str, &str),
    ) -> Option<ConfirmedAccount> {
        let record = self.connected_record(person).await?;
        record
            .accounts
            .get(&entry_id(app, issuer))
            .map(|kept| ConfirmedAccount {
                id: kept.account.clone(),
                name: kept.name.clone(),
            })
    }

    /// Every account the person `person` (their id) connected, for them to see and forget.
    pub async fn connected_accounts(&self, person: &str) -> Vec<ConnectedAccount> {
        let Some(record) = self.connected_record(person).await else {
            return Vec::new();
        };
        let mut listed: Vec<ConnectedAccount> = record.accounts.values().map(shown).collect();
        listed.sort_by(|a, b| (&a.app, &a.issuer).cmp(&(&b.app, &b.issuer)));
        listed
    }

    /// Answer `request` when it is one of a person's connected accounts' actions
    /// ([`CONNECT_ACCOUNT`], [`FINISH_CONNECT_ACCOUNT`], [`FORGET_CONNECTED_ACCOUNT`]), asked
    /// in a workspace `workspace_id` names; `None` for any other action.
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
        if workspace_id.is_none() {
            return Some(Err(ActionCallError::UnknownAction));
        }
        Some(self.connected_invoke(caller, request).await)
    }

    async fn connected_invoke(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        if request.target.address.action == FORGET_CONNECTED_ACCOUNT {
            let person = forgetter(caller)?;
            let forgetting: ForgetConnectedAccount =
                super::argument(request, "the app and issuer of the account to forget")?;
            return super::answer(&self.forget_connected(&person, &forgetting, request).await);
        }
        let (person, app) = asker(caller)?;
        match request.target.address.action.as_str() {
            CONNECT_ACCOUNT => {
                let asking: ConnectAccount =
                    super::argument(request, "an issuer to connect an account at")?;
                super::answer(&self.connect_account((&person, &app), asking, request).await)
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
        (person, app): (&str, &str),
        asking: ConnectAccount,
        call: &InvokeRequest,
    ) -> Result<SignInStep, String> {
        // Where the account would be kept is settled before the browser goes anywhere.
        self.changing_connected(person).await?;
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
                    let described = Described { issuer, client_id };
                    workspaces
                        .keep_connected((&person, &app), &call, (described, account, &tokens))
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

    /// Keep what a connect confirmed in `person`'s own workspace: the grant in its vault, in
    /// place of the one an earlier connect of the same account kept, and the account in the
    /// record of their connected accounts.
    async fn keep_connected(
        &self,
        (person, app): (&str, &str),
        call: &InvokeRequest,
        (described, account, tokens): (Described, ConfirmedAccount, &tilo_oauth::TokenSet),
    ) -> Result<ConnectedAccount, String> {
        let workspace = self.changing_connected(person).await?;
        let issuer = described.issuer.id.clone();
        let earlier = self
            .connected_record(person)
            .await
            .and_then(|record| {
                record
                    .accounts
                    .get(&entry_id(app, &issuer))
                    .map(|kept| kept.grant.clone())
            })
            .and_then(|grant| ItemId::parse(&grant));
        let place =
            Place::parse(&described.issuer.token_endpoint).map_err(|_| NOT_KEPT.to_owned())?;
        let grant = serde_json::to_string(tokens).map_err(|_| NOT_KEPT.to_owned())?;
        let acting = tilo_action_policy::Acting::person(person.to_owned());
        let name = account.name.clone();
        let item = self
            .vault_for(&workspace, &acting)
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
        let kept = Connected {
            app: app.to_owned(),
            issuer,
            account: account.id,
            name: account.name,
            described: serde_json::to_string(&described).map_err(|_| NOT_KEPT.to_owned())?,
            grant: item,
        };
        let shown = shown(&kept);
        person_workspace::apply_on(
            self,
            &workspace,
            CONNECTED,
            ConnectedAccounts::KEEP.with((
                kept.app,
                kept.issuer,
                kept.account,
                kept.name,
                kept.described,
                kept.grant,
            )),
        )
        .await
        .map_err(|error| {
            tracing::warn!(%error, "a connected account was not kept in its person's workspace");
            NOT_KEPT.to_owned()
        })?;
        self.person_settings_changed();
        Ok(shown)
    }
}

impl Workspaces {
    /// Forget the account `person` connected at `forgetting`'s issuer: the grant leaves their
    /// vault, the issuer is given it back where it takes one back, and nothing of it is kept.
    /// Answers the accounts still connected.
    async fn forget_connected(
        &self,
        person: &str,
        forgetting: &ForgetConnectedAccount,
        call: &InvokeRequest,
    ) -> Result<Vec<ConnectedAccount>, String> {
        let (app, issuer) = (forgetting.app.as_str(), forgetting.issuer.as_str());
        let workspace = self.changing_connected(person).await?;
        let kept = self
            .connected_record(person)
            .await
            .and_then(|record| record.accounts.get(&entry_id(app, issuer)).cloned())
            .ok_or_else(|| NOT_CONNECTED.to_owned())?;
        let described = described_of(&kept);
        let acting = tilo_action_policy::Acting::person(person.to_owned());
        let place = described.as_ref().and_then(|described| {
            match Place::parse(&described.issuer.token_endpoint) {
                Ok(place) => Some(place),
                Err(error) => {
                    tracing::warn!(%error, "a connected account's token endpoint names no place");
                    None
                }
            }
        });
        let grant = match (ItemId::parse(&kept.grant), place) {
            (Some(id), Some(place)) => self
                .vault_for(&workspace, &acting)
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
        person_workspace::apply_on(
            self,
            &workspace,
            CONNECTED,
            ConnectedAccounts::FORGET.with((app.to_owned(), issuer.to_owned())),
        )
        .await
        .map_err(|error| {
            tracing::warn!(%error, "a forgotten connected account stays kept");
            "The account could not be forgotten here. Try again".to_owned()
        })?;
        if let Some(released) = grant
            && let Some(described) = described
            && let Ok(tokens) = serde_json::from_str::<tilo_oauth::TokenSet>(&released.secret)
        {
            give_back(described, tokens).await;
        }
        self.person_settings_changed();
        Ok(self.connected_accounts(person).await)
    }
}

/// Give a forgotten account's grant back to its issuer, where the issuer takes one back. The
/// account is forgotten here either way; an issuer that did not answer keeps a grant nobody
/// holds any more, which runs out on its own.
async fn give_back(described: Described, tokens: tilo_oauth::TokenSet) {
    if described.issuer.revocation_endpoint.is_none() {
        return;
    }
    let given = tokio::task::spawn_blocking(move || {
        let client = tilo_oauth::client::OAuthClient::new()?;
        client.revoke(
            &described.issuer.provider(),
            &described.issuer.registration(&described.client_id),
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
