//! Signing in on a provider's own page: a Nextcloud server's login flow, a
//! drive's `OAuth` page, and an `Apple ID` with the verification code Apple
//! sends.
//!
//! A Nextcloud flow is started ([`BEGIN_SIGN_IN`]), the person's browser shows
//! its page, and it is polled in the background until the person approved
//! access there or the flow ran out. A drive's page is opened from a loopback
//! this host waits on, or the code the provider shows is pasted back
//! ([`SUBMIT_SIGN_IN_CODE`]); `iCloud` asks for the code Apple sent. What the
//! sign-in leaves — an app password, a grant, an `iCloud` session — is kept
//! in the workspace's vault as the account's own entry
//! ([`super::kept_sign_ins`]).
//!
//! What is begun is kept in memory alone, one entry per sign-in, expiring;
//! no secret is ever answered to a caller.
//!
//! [`BEGIN_SIGN_IN`]: tilo_actions::remote::person_accounts::BEGIN_SIGN_IN
//! [`SUBMIT_SIGN_IN_CODE`]: tilo_actions::remote::person_accounts::SUBMIT_SIGN_IN_CODE

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, mpsc};
use std::time::Duration;

use account_credentials::icloud::{ICloudAuth, ICloudChallenge, ICloudSignIn, WebICloud};
use account_credentials::nextcloud;
use account_credentials::{
    Authorization, CredentialStore, OAuthCloud, OAuthIssuer, icloud_secret, oauth_secret,
};
use serde_json::{Value, json};
use tilo_actions::remote::person_accounts::{
    BeginCloudSignIn, BeginSignIn, CancelSignIn, CloudKind, Provider, Services, SignInStep,
    SigningIn, SubmitSignInCode,
};
use tilo_net::Url;
use zeroize::Zeroizing;

use super::super::vault::VaultAt;
use super::kept_sign_ins::{SIGN_IN, places_of, sign_in_of};
use crate::workspaces::Workspaces;

/// How long a Nextcloud login flow may stay open.
const NEXTCLOUD_MINUTES: u64 = 20;
/// How long any other sign-in may stay open.
const SIGN_IN_MINUTES: u64 = 20;
/// The page an `Apple ID`'s sign-in is begun from.
const APPLE_SIGN_IN: &str = "https://www.icloud.com";
/// How often a Nextcloud server is asked whether the person approved.
const POLL_EVERY: Duration = Duration::from_secs(2);
/// The most sign-ins begun and not forgotten at once.
const MAX_BEGUN: usize = 64;
/// The account type a calendar server is kept as, in the workspace's accounts.
const CALDAV_TYPE: &str = "caldav";

/// A sign-in begun and not finished.
struct Begun {
    id: String,
    person: [u8; 32],
    /// Where the workspace this sign-in keeps its account in keeps what is its own.
    root: PathBuf,
    until_ms: u64,
    failed: Option<String>,
    flow: Flow,
}

/// What a begun sign-in follows.
enum Flow {
    /// A Nextcloud login flow, polled in the background.
    Nextcloud {
        /// The Nextcloud server it signs in to.
        server: String,
        /// Held by the poll; dropping it stops it.
        stop: Option<mpsc::Sender<()>>,
    },
    /// A cloud provider's own page: either the loopback waits for the
    /// browser, or the code it shows is pasted.
    Oauth {
        cloud: OAuthCloud,
        name: String,
        /// The sign-in awaiting the code the provider showed; `None` while
        /// the loopback waits for the browser instead.
        pasted: Option<Box<Authorization>>,
        /// Stops the loopback wait.
        stop: Option<account_credentials::LoopbackCancel>,
    },
    /// A mailbox's provider page, signed in with the grant it leaves: Google's
    /// or Microsoft's own, kept as a mail account once the browser approves it.
    OauthMail {
        provider: Provider,
        /// Stops the loopback wait.
        stop: Option<account_credentials::LoopbackCancel>,
    },
    /// An `Apple ID` signed in to, waiting for the verification code Apple
    /// sent to the person's devices.
    Icloud {
        challenge: ICloudChallenge,
        name: String,
        apple_id: String,
    },
}

/// Every sign-in begun and not yet forgotten, by its id.
static BEGUN: Mutex<BTreeMap<String, Begun>> = Mutex::new(BTreeMap::new());

fn now_ms() -> u64 {
    tilo_clock::unix_now_ns() / 1_000_000
}

fn minutes_ms(minutes: u64) -> u64 {
    minutes.saturating_mul(60_000)
}

fn begun() -> std::sync::MutexGuard<'static, BTreeMap<String, Begun>> {
    BEGUN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The providers this server can sign a person in with on their own page: Google's and
/// Microsoft's for a mailbox, and Nextcloud's login flow. A build without a provider's client
/// registration refuses its sign-in in words its person reads.
#[must_use]
pub(super) fn signs_in_with() -> Vec<Provider> {
    vec![Provider::Google, Provider::Microsoft, Provider::Nextcloud]
}

const fn provider_label(provider: Provider) -> &'static str {
    match provider {
        Provider::Google => "Google",
        Provider::Microsoft => "Microsoft",
        Provider::Icloud => "iCloud",
        Provider::Nextcloud => "Nextcloud",
        Provider::Other => "That provider",
    }
}

/// What the person signs in for: what they chose, or, when they chose
/// nothing, what the provider is used for by default.
fn chosen(provider: Provider, services: Services) -> Services {
    let chosen = if services.none() {
        Services::default()
    } else {
        services
    }
    .within(provider);
    if !chosen.none() {
        return chosen;
    }
    match provider {
        Provider::Nextcloud => Services {
            mail: false,
            calendar: true,
            files: false,
        },
        Provider::Google | Provider::Microsoft | Provider::Icloud | Provider::Other => {
            services.within(provider)
        }
    }
}

/// The sign-ins begun by `person` that made no account yet, oldest first.
pub(super) fn signing_in(root: &Path) -> Vec<SigningIn> {
    prune();
    begun()
        .values()
        .filter(|entry| entry.root == root)
        .filter_map(|entry| {
            let (provider, server) = match &entry.flow {
                Flow::Nextcloud { server, .. } => (Provider::Nextcloud, server.clone()),
                Flow::OauthMail { provider, .. } => (*provider, String::new()),
                Flow::Oauth { .. } | Flow::Icloud { .. } => return None,
            };
            Some(SigningIn {
                id: entry.id.clone(),
                provider,
                server,
                failed: entry.failed.clone(),
            })
        })
        .collect()
}

/// Forget every sign-in that ran out.
fn prune() {
    let now = now_ms();
    begun().retain(|_, entry| entry.until_ms > now);
}

impl Workspaces {
    /// Begin signing in on `asking`'s provider page for `person`, into the
    /// workspace `workspace`, answering where their browser goes. What
    /// reaches the workspace's accounts is what [`BEGIN_SIGN_IN`] says:
    /// nothing yet.
    ///
    /// # Errors
    /// The provider is not one this server signs in with, the server it
    /// would ask is one it may not call, or too many sign-ins are already
    /// begun.
    ///
    /// [`BEGIN_SIGN_IN`]: tilo_actions::remote::person_accounts::BEGIN_SIGN_IN
    pub(super) async fn begin_sign_in(
        &self,
        (person, workspace, call): ([u8; 32], &str, Option<&tilo_actions::remote::InvokeRequest>),
        asking: &BeginSignIn,
        reach: &tilo_account_probe::imap::Reach,
    ) -> Result<SignInStep, String> {
        let provider = asking.provider;
        let services = chosen(provider, asking.services());
        prune();
        if begun().len() >= MAX_BEGUN {
            return Err("Too many sign-ins are open here. Finish or cancel one first".into());
        }
        match provider {
            Provider::Nextcloud => {
                self.begin_nextcloud((person, workspace, call), services, asking, reach)
                    .await
            }
            Provider::Google | Provider::Microsoft => {
                self.begin_oauth_mail((person, workspace, call), provider, asking)
            }
            Provider::Icloud | Provider::Other => Err(format!(
                "{} signs in with a password, not on its own page",
                provider_label(provider)
            )),
        }
    }

    async fn begin_nextcloud(
        &self,
        (person, workspace, call): ([u8; 32], &str, Option<&tilo_actions::remote::InvokeRequest>),
        services: Services,
        asking: &BeginSignIn,
        reach: &tilo_account_probe::imap::Reach,
    ) -> Result<SignInStep, String> {
        let server = asking.server.trim().to_owned();
        if server.is_empty() {
            return Err("Enter the address of the Nextcloud server".into());
        }
        let parsed = nextcloud::server_url(&server).map_err(|error| error.to_string())?;
        let named = named_by(&parsed)?;
        let lookup = reach.lookup();
        if reach.is_public() {
            let answers = lookup(named).await.map_err(|error| {
                if tilo_net::address::not_public(&error) {
                    "That server's address is not one this server may call".to_owned()
                } else {
                    "No server was found under that name".to_owned()
                }
            })?;
            if answers.is_empty() {
                return Err("No server was found under that name".into());
            }
        }
        let flow = tokio::task::spawn_blocking({
            let server = server.clone();
            move || nextcloud::begin_looking(&server, lookup).map_err(|error| error.to_string())
        })
        .await
        .map_err(|_| "the Nextcloud server was not asked to the end".to_string())??;
        let id = tilo_oauth::random_token().map_err(|error| error.to_string())?;
        let open = flow.login_url().to_owned();
        let (stop, cancelled) = nextcloud::cancellation();
        let account = asking.account.trim().to_owned();
        begun().insert(
            id.clone(),
            Begun {
                id: id.clone(),
                person,
                root: self.workspace_root(workspace),
                until_ms: now_ms().saturating_add(minutes_ms(NEXTCLOUD_MINUTES)),
                failed: None,
                flow: Flow::Nextcloud {
                    server,
                    stop: Some(stop),
                },
            },
        );
        let workspaces = self.clone();
        let workspace = workspace.to_owned();
        let asker = tilo_action_policy::Acting::person(hex::encode(person));
        // The login is kept as the call that began the sign-in asked, once the server sends it.
        let call = call.cloned();
        let kept_id = id.clone();
        let cancelled_at = now_ms().saturating_add(minutes_ms(NEXTCLOUD_MINUTES));
        tokio::spawn(async move {
            let waited = tokio::task::spawn_blocking(move || {
                flow.wait(
                    &cancelled,
                    POLL_EVERY,
                    Duration::from_secs(minutes_ms(NEXTCLOUD_MINUTES) / 1000),
                )
                .map_err(|error| error.to_string())
            })
            .await;
            let waited = match waited {
                Ok(Ok(login)) => Ok(Box::pin(keep_nextcloud(
                    workspaces.vault_serving(&workspace, &asker, call.as_ref()),
                    &login,
                    services,
                    &account,
                ))
                .await),
                Ok(Err(reason)) => Ok(Err(reason)),
                Err(error) => Err(error),
            };
            match waited {
                Ok(Ok(())) => {
                    begun().remove(&kept_id);
                    workspaces.person_settings_changed();
                }
                Ok(Err(reason)) => keep_failed(&kept_id, &reason, cancelled_at),
                Err(_) => keep_failed(&kept_id, "the Nextcloud answer was not read", cancelled_at),
            }
        });
        Ok(SignInStep {
            id,
            open,
            code: String::new(),
        })
    }

    /// Begin signing in to a mailbox on Google's or Microsoft's own page: the loopback waits
    /// for the browser, and the grant it leaves is kept as a mail account whose sign-in it is.
    fn begin_oauth_mail(
        &self,
        (person, workspace, call): ([u8; 32], &str, Option<&tilo_actions::remote::InvokeRequest>),
        provider: Provider,
        asking: &BeginSignIn,
    ) -> Result<SignInStep, String> {
        let issuer = match provider {
            Provider::Google => OAuthIssuer::Google,
            Provider::Microsoft => OAuthIssuer::Microsoft,
            _ => return Err("That provider signs in with a password".into()),
        };
        let address = asking.address.trim().to_owned();
        if provider == Provider::Microsoft && address.is_empty() {
            return Err(
                "Type the address you sign in to Microsoft with, so the account is kept under it"
                    .into(),
            );
        }
        let root = self.workspace_root(workspace);
        let store = self.cloud_credentials(&root);
        let hint = (!address.is_empty()).then_some(address.as_str());
        let authorization = store
            .begin_sign_in_for_capabilities(issuer, &[account_credentials::Capability::Mail], hint)
            .map_err(|error| match error {
                account_credentials::CredentialError::NotConfigured { provider, .. } => {
                    format!("Signing in with {provider} is not set up in this build")
                }
                error => error.to_string(),
            })?;
        let Some(stop) = authorization.canceller() else {
            return Err(format!(
                "{}'s sign-in could not wait for your browser here. Try again",
                issuer.label()
            ));
        };
        let open = authorization.url().to_owned();
        let id = tilo_oauth::random_token().map_err(|error| error.to_string())?;
        let until_ms = now_ms().saturating_add(minutes_ms(SIGN_IN_MINUTES));
        let name = mail_sign_in_name(issuer, asking, &address);
        let services = mail_services();
        begun().insert(
            id.clone(),
            Begun {
                id: id.clone(),
                person,
                root,
                until_ms,
                failed: None,
                flow: Flow::OauthMail {
                    provider,
                    stop: Some(stop),
                },
            },
        );
        let workspaces = self.clone();
        let workspace = workspace.to_owned();
        let tester = self.account_tester();
        let replace = asking.account.trim().to_owned();
        let kept_id = id.clone();
        let cancelled_at = until_ms;
        // The sign-in is kept as the call that began it asked, once the provider approves it.
        let call = call.cloned();
        tokio::spawn(async move {
            let finished =
                tokio::task::spawn_blocking(move || CredentialStore::finish_sign_in(authorization))
                    .await;
            let kept = match finished {
                Ok(Ok(tokens)) => {
                    let asker = tilo_action_policy::Acting::person(hex::encode(person));
                    let vault = workspaces.vault_for(&workspace, &asker);
                    let vault = call.as_ref().map_or(vault, |call| vault.serving(call));
                    Box::pin(keep_oauth_mail(
                        vault,
                        Grant {
                            store: &store,
                            tokens: &tokens,
                        },
                        issuer,
                        &name,
                        &address,
                        &replace,
                        services,
                        tester,
                    ))
                    .await
                }
                Ok(Err(error)) => Err(error.to_string()),
                Err(_) => Err("the sign-in was not read to the end".to_owned()),
            };
            match kept {
                Ok(()) => {
                    begun().remove(&kept_id);
                    workspaces.person_settings_changed();
                }
                Err(reason) => keep_failed(&kept_id, &reason, cancelled_at),
            }
        });
        Ok(SignInStep {
            id,
            open,
            code: String::new(),
        })
    }

    /// Begin the sign-in a request asks for, and wake the windows following
    /// the accounts once it is listed.
    pub(super) async fn begin_account_sign_in(
        &self,
        (person, workspace, call): ([u8; 32], &str, Option<&tilo_actions::remote::InvokeRequest>),
        asking: &BeginSignIn,
    ) -> Result<SignInStep, String> {
        let reach = self.inner.models.mail_reach();
        let step = self
            .begin_sign_in((person, workspace, call), asking, &reach)
            .await;
        if step.is_ok() {
            self.person_settings_changed();
        }
        step
    }

    /// Stop waiting for a sign-in, or forget one that failed, and answer the
    /// accounts as they now are.
    pub(super) async fn cancel_account_sign_in(
        &self,
        person: [u8; 32],
        stopping: &CancelSignIn,
        (workspace, root): (&str, &Path),
    ) -> Result<super::MailAccounts, String> {
        cancel(&person, stopping)?;
        let listed = super::mail_accounts(&self.workspace_accounts(workspace).await?, root);
        self.person_settings_changed();
        Ok(listed)
    }

    /// Begin connecting a cloud-storage account for `person` in `workspace`
    /// ([`BEGIN_CLOUD_SIGN_IN`]), answering where their browser goes and what
    /// the sign-in still waits for.
    ///
    /// # Errors
    /// The provider is not configured in this build, its server is one this
    /// host may not call, the person typed nothing, or too many sign-ins are
    /// already begun.
    pub(super) async fn begin_cloud_sign_in(
        &self,
        (person, workspace): ([u8; 32], &str),
        call: Option<&tilo_actions::remote::InvokeRequest>,
        asking: &BeginCloudSignIn,
    ) -> Result<SignInStep, String> {
        prune();
        if begun().len() >= MAX_BEGUN {
            return Err("Too many sign-ins are open here. Finish or cancel one first".into());
        }
        let step = match asking.kind {
            CloudKind::GoogleDrive | CloudKind::OneDrive | CloudKind::Dropbox => {
                self.begin_oauth_cloud((person, workspace), call, asking)
            }
            CloudKind::IcloudDrive => {
                Box::pin(self.begin_icloud_cloud((person, workspace), call, asking)).await
            }
            CloudKind::Webdav => {
                Box::pin(self.begin_webdav_cloud((person, workspace), call, asking)).await
            }
        }?;
        self.person_settings_changed();
        Ok(step)
    }

    /// Begin a drive's sign-in on its provider's page: the loopback waits for
    /// the browser, or, where no port is free, the person pastes the code the
    /// provider shows ([`SubmitSignInCode`]).
    fn begin_oauth_cloud(
        &self,
        (person, workspace): ([u8; 32], &str),
        call: Option<&tilo_actions::remote::InvokeRequest>,
        asking: &BeginCloudSignIn,
    ) -> Result<SignInStep, String> {
        let cloud = match asking.kind {
            CloudKind::GoogleDrive => OAuthCloud::GoogleDrive,
            CloudKind::OneDrive => OAuthCloud::OneDrive,
            CloudKind::Dropbox => OAuthCloud::Dropbox,
            CloudKind::IcloudDrive | CloudKind::Webdav => {
                return Err("That storage does not sign in on its provider's page".into());
            }
        };
        let root = self.workspace_root(workspace);
        let store = self.cloud_credentials(&root);
        let authorization = store
            .begin_sign_in(cloud, None)
            .map_err(|error| match error {
                account_credentials::CredentialError::NotConfigured { provider, .. } => {
                    format!("Signing in with {provider} is not set up in this build")
                }
                error => error.to_string(),
            })?;
        let open = authorization.url().to_owned();
        let name = cloud_name(asking, cloud.label());
        let id = tilo_oauth::random_token().map_err(|error| error.to_string())?;
        let until_ms = now_ms().saturating_add(minutes_ms(SIGN_IN_MINUTES));
        let code = if authorization.needs_pasted_code() {
            let prompt = format!(
                "Copy the code {} shows and paste it here.",
                cloud.sign_in_with()
            );
            begun().insert(
                id.clone(),
                Begun {
                    id: id.clone(),
                    person,
                    root,
                    until_ms,
                    failed: None,
                    flow: Flow::Oauth {
                        cloud,
                        name,
                        pasted: Some(Box::new(authorization)),
                        stop: None,
                    },
                },
            );
            prompt
        } else {
            let stop = authorization.canceller();
            begun().insert(
                id.clone(),
                Begun {
                    id: id.clone(),
                    person,
                    root: root.clone(),
                    until_ms,
                    failed: None,
                    flow: Flow::Oauth {
                        cloud,
                        name: name.clone(),
                        pasted: None,
                        stop,
                    },
                },
            );
            let workspaces = self.clone();
            let workspace = workspace.to_owned();
            let kept_id = id.clone();
            let cancelled_at = until_ms;
            // The drive is kept as the call that began the sign-in asked, once the page approves.
            let call = call.cloned();
            tokio::spawn(async move {
                let finished = tokio::task::spawn_blocking(move || {
                    CredentialStore::finish_sign_in(authorization)
                })
                .await;
                let kept = match finished {
                    Ok(Ok(tokens)) => {
                        let asker = tilo_action_policy::Acting::person(hex::encode(person));
                        let vault = workspaces.vault_for(&workspace, &asker);
                        let vault = call.as_ref().map_or(vault, |call| vault.serving(call));
                        Box::pin(keep_oauth_account(vault, cloud, &name, &tokens, &root)).await
                    }
                    Ok(Err(error)) => Err(error.to_string()),
                    Err(_) => Err("the sign-in was not read to the end".to_owned()),
                };
                match kept {
                    Ok(()) => {
                        begun().remove(&kept_id);
                        workspaces.person_settings_changed();
                    }
                    Err(reason) => keep_failed(&kept_id, &reason, cancelled_at),
                }
            });
            String::new()
        };
        Ok(SignInStep { id, open, code })
    }

    /// Begin an `Apple ID`'s sign-in: Apple sends a verification code to the
    /// person's devices, which [`SubmitSignInCode`] finishes the sign-in with.
    async fn begin_icloud_cloud(
        &self,
        (person, workspace): ([u8; 32], &str),
        call: Option<&tilo_actions::remote::InvokeRequest>,
        asking: &BeginCloudSignIn,
    ) -> Result<SignInStep, String> {
        let apple_id = asking.username.trim().to_owned();
        let password = Zeroizing::new(asking.password.clone());
        if apple_id.is_empty() || password.is_empty() {
            return Err("Enter your Apple ID and its password".into());
        }
        let name = cloud_name(asking, CloudKind::IcloudDrive.label());
        let auth = tokio::task::spawn_blocking({
            let workspaces = self.clone();
            move || workspaces.icloud_auth()
        })
        .await
        .map_err(|_| "the Apple ID sign-in did not start".to_owned())??;
        let root = self.workspace_root(workspace);
        let signing_in = apple_id.clone();
        let outcome =
            tokio::task::spawn_blocking(move || auth.sign_in(&signing_in, password.as_str()))
                .await
                .map_err(|_| "the Apple ID sign-in did not finish".to_owned())?
                .map_err(|error| error.to_string())?;
        match outcome {
            ICloudSignIn::NeedsCode(challenge) => {
                let id = tilo_oauth::random_token().map_err(|error| error.to_string())?;
                begun().insert(
                    id.clone(),
                    Begun {
                        id: id.clone(),
                        person,
                        root,
                        until_ms: now_ms().saturating_add(minutes_ms(SIGN_IN_MINUTES)),
                        failed: None,
                        flow: Flow::Icloud {
                            challenge,
                            name: name.clone(),
                            apple_id: apple_id.clone(),
                        },
                    },
                );
                Ok(SignInStep {
                    id,
                    open: APPLE_SIGN_IN.to_owned(),
                    code: "Enter the verification code Apple sent to your devices.".to_owned(),
                })
            }
            ICloudSignIn::Complete(session) => {
                let asker = tilo_action_policy::Acting::person(hex::encode(person));
                let vault = self.vault_for(workspace, &asker);
                let vault = call.map_or(vault, |call| vault.serving(call));
                keep_icloud_account(vault, &name, &apple_id, &session).await?;
                Ok(SignInStep {
                    id: String::new(),
                    open: APPLE_SIGN_IN.to_owned(),
                    code: String::new(),
                })
            }
        }
    }

    /// Connect a `WebDAV` server from the address and sign-in the person
    /// typed: nothing opens in a browser.
    async fn begin_webdav_cloud(
        &self,
        (person, workspace): ([u8; 32], &str),
        call: Option<&tilo_actions::remote::InvokeRequest>,
        asking: &BeginCloudSignIn,
    ) -> Result<SignInStep, String> {
        let server = asking.server.trim().to_owned();
        let username = asking.username.trim().to_owned();
        let password = Zeroizing::new(asking.password.clone());
        if server.is_empty() || username.is_empty() || password.is_empty() {
            return Err("Enter the WebDAV server, its user name and its password".into());
        }
        let parsed = Url::parse(&server).map_err(|_| "That is no server address".to_owned())?;
        if parsed.scheme() != "https" && parsed.scheme() != "http" {
            return Err("A WebDAV server's address begins with https://".into());
        }
        let name = cloud_name(asking, CloudKind::Webdav.label());
        let asker = tilo_action_policy::Acting::person(hex::encode(person));
        let vault = self.vault_for(workspace, &asker);
        let vault = call.map_or(vault, |call| vault.serving(call));
        keep_webdav_account(vault, &name, &server, &username, &password).await?;
        Ok(SignInStep {
            id: String::new(),
            open: String::new(),
            code: String::new(),
        })
    }

    /// Finish the sign-in `submitting` names with the code it carries
    /// ([`SUBMIT_SIGN_IN_CODE`]), keeping the account it signs in to.
    ///
    /// # Errors
    /// No such sign-in was begun here, it waits for no code, or the code the
    /// provider was given was refused.
    pub(super) async fn submit_sign_in_code(
        &self,
        person: [u8; 32],
        (workspace, root): (&str, &Path),
        call: Option<&tilo_actions::remote::InvokeRequest>,
        submitting: &SubmitSignInCode,
    ) -> Result<super::MailAccounts, String> {
        let theirs = begun()
            .get(&submitting.sign_in)
            .is_some_and(|entry| entry.person == person);
        if !theirs {
            return Err("That sign-in was not begun here".into());
        }
        let code = submitting.code.trim().to_owned();
        if code.is_empty() {
            return Err("Enter the code the provider sent or showed".into());
        }
        let acting = tilo_action_policy::Acting::person(hex::encode(person));
        // What this finishes is kept as the call that completes it asked.
        let vault = call.map_or_else(
            || self.vault_for(workspace, &acting),
            |call| self.vault_for(workspace, &acting).serving(call),
        );
        let finish = {
            let mut entries = begun();
            let Some(entry) = entries.get_mut(&submitting.sign_in) else {
                return Err("That sign-in was not begun here".into());
            };
            let finish = match &mut entry.flow {
                Flow::Icloud {
                    challenge,
                    name,
                    apple_id,
                } => Finish::Icloud(
                    ICloudChallenge(Zeroizing::new(challenge.0.to_string())),
                    name.clone(),
                    apple_id.clone(),
                ),
                Flow::Oauth {
                    cloud,
                    name,
                    pasted,
                    ..
                } => match pasted.take() {
                    Some(authorization) => Finish::Pasted(authorization, *cloud, name.clone()),
                    None => Finish::None,
                },
                Flow::Nextcloud { .. } | Flow::OauthMail { .. } => Finish::None,
            };
            drop(entries);
            finish
        };
        match finish {
            Finish::Icloud(challenge, name, apple_id) => {
                finish_icloud_sign_in(
                    self,
                    vault,
                    (challenge, name, apple_id),
                    (&code, &submitting.sign_in),
                )
                .await?;
            }
            Finish::Pasted(authorization, cloud, name) => {
                let finished = tokio::task::spawn_blocking(move || {
                    CredentialStore::finish_sign_in_with_code(*authorization, &code)
                })
                .await
                .map_err(|_| "the code was not read to the end".to_owned())?;
                let tokens = match finished {
                    Ok(tokens) => tokens,
                    Err(error) => {
                        begun().remove(&submitting.sign_in);
                        return Err(format!("{error}. Begin the sign-in again"));
                    }
                };
                keep_oauth_account(vault, cloud, &name, &tokens, root).await?;
                begun().remove(&submitting.sign_in);
            }
            Finish::None => {
                return Err("That sign-in does not wait for a code".to_owned());
            }
        }
        self.person_settings_changed();
        Ok(super::mail_accounts(
            &self.workspace_accounts(workspace).await?,
            root,
        ))
    }

    /// What a sign-in with a provider's own page is begun with.
    #[cfg_attr(
        not(any(test, feature = "test-support")),
        allow(
            clippy::unused_self,
            reason = "a test signs in through a stand-in provider; a real build uses the committed registrations"
        )
    )]
    pub(in crate::workspaces) fn cloud_credentials(&self, root: &Path) -> CredentialStore {
        #[cfg(any(test, feature = "test-support"))]
        {
            let stood_in = self
                .inner
                .cloud_sign_in
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if let Some(store) = stood_in {
                return store;
            }
        }
        CredentialStore::for_root(Some(root.to_path_buf()))
    }

    /// What an `Apple ID` signs in with: Apple's own, or a test's stand-in.
    #[cfg_attr(
        not(any(test, feature = "test-support")),
        allow(
            clippy::unused_self,
            reason = "a test signs in through a stand-in provider; a real build uses Apple's"
        )
    )]
    fn icloud_auth(&self) -> Result<std::sync::Arc<dyn ICloudAuth>, String> {
        #[cfg(any(test, feature = "test-support"))]
        {
            let stood_in = self
                .inner
                .icloud_sign_in
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if let Some(auth) = stood_in {
                return Ok(auth);
            }
        }
        WebICloud::apple()
            .map(|auth| std::sync::Arc::new(auth) as std::sync::Arc<dyn ICloudAuth>)
            .map_err(|error| error.to_string())
    }
}

/// What a code a person submits finishes: a sign-in of the provider's own page, keeping the
/// grant it returns, or nothing that waits for a code.
enum Finish {
    Icloud(ICloudChallenge, String, String),
    Pasted(Box<Authorization>, OAuthCloud, String),
    None,
}

/// The name a mailbox's account is shown as: what the person typed, the address it signs in at,
/// or its provider's own name.
fn mail_sign_in_name(issuer: OAuthIssuer, asking: &BeginSignIn, address: &str) -> String {
    if !asking.name.trim().is_empty() {
        return asking.name.trim().to_owned();
    }
    if address.is_empty() {
        issuer.label().to_owned()
    } else {
        address.to_owned()
    }
}

/// The services a mailbox's sign-in keeps: its mail alone, the provider's others asked for
/// later, from the account's row.
const fn mail_services() -> Services {
    Services {
        mail: true,
        calendar: false,
        files: false,
    }
}

/// The name a cloud account is shown under: what the person typed, else its
/// provider's.
fn cloud_name(asking: &BeginCloudSignIn, provider: &str) -> String {
    let name = asking.name.trim();
    if name.is_empty() {
        provider.to_owned()
    } else {
        name.to_owned()
    }
}

/// The id a cloud account is kept under: its type and a slug of its name, so
/// signing the same storage in again replaces its account rather than adding
/// one beside it.
fn cloud_id(type_id: &str, name: &str) -> String {
    let mut slug = String::new();
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "account" } else { slug };
    format!("{type_id}:{slug}")
}

/// Keep a sign-in's failure with it, so the person reads why and can cancel
/// it, and stop a Nextcloud poll that is still running.
fn keep_failed(id: &str, reason: &str, until_ms: u64) {
    let mut entries = begun();
    if let Some(entry) = entries.get_mut(id) {
        entry.failed = Some(reason.to_owned());
        entry.until_ms = until_ms;
        match &mut entry.flow {
            Flow::Nextcloud { stop, .. } => *stop = None,
            Flow::Oauth { stop, .. } | Flow::OauthMail { stop, .. } => *stop = None,
            Flow::Icloud { .. } => {}
        }
    }
}

/// Answer a request of the sign-in actions: where a browser goes, or the
/// accounts as they are once a sign-in was stopped.
///
/// # Errors
/// The request does not read, or names a provider or sign-in this server
/// does not know.
pub(super) async fn accounts_sign_in(
    workspaces: &Workspaces,
    person: [u8; 32],
    workspace: &str,
    request: &tilo_actions::remote::InvokeRequest,
) -> Result<serde_json::Value, tilo_actions::remote::ActionCallError> {
    use tilo_actions::remote::person_accounts::{
        BEGIN_CLOUD_SIGN_IN, BEGIN_SIGN_IN, CANCEL_SIGN_IN, SUBMIT_SIGN_IN_CODE,
    };

    match request.target.address.action.as_str() {
        BEGIN_SIGN_IN => {
            let asking: BeginSignIn = super::argument(request, "a provider to sign in with")?;
            super::answer(
                &workspaces
                    .begin_account_sign_in((person, workspace, Some(request)), &asking)
                    .await,
            )
        }
        BEGIN_CLOUD_SIGN_IN => {
            let asking: BeginCloudSignIn = super::argument(request, "storage to connect")?;
            super::answer(
                &workspaces
                    .begin_cloud_sign_in((person, workspace), Some(request), &asking)
                    .await,
            )
        }
        SUBMIT_SIGN_IN_CODE => {
            let submitting: SubmitSignInCode = super::argument(request, "a sign-in's code")?;
            super::answer(
                &workspaces
                    .submit_sign_in_code(
                        person,
                        (workspace, &workspaces.workspace_root(workspace)),
                        Some(request),
                        &submitting,
                    )
                    .await,
            )
        }
        CANCEL_SIGN_IN => {
            let stopping: CancelSignIn = super::argument(request, "a sign-in to stop")?;
            super::answer(
                &workspaces
                    .cancel_account_sign_in(
                        person,
                        &stopping,
                        (workspace, &workspaces.workspace_root(workspace)),
                    )
                    .await,
            )
        }
        _ => Err(tilo_actions::remote::ActionCallError::UnknownAction),
    }
}

/// Stop waiting for the sign-in `cancel` names, or forget one that failed.
///
/// # Errors
/// No such sign-in was begun by this person.
pub(super) fn cancel(person: &[u8; 32], cancel: &CancelSignIn) -> Result<(), String> {
    let mut entries = begun();
    let theirs = entries
        .get(&cancel.sign_in)
        .is_some_and(|entry| &entry.person == person);
    if !theirs {
        return Err("That sign-in was not begun here".into());
    }
    if let Some(entry) = entries.remove(&cancel.sign_in) {
        match entry.flow {
            Flow::Oauth {
                stop: Some(stop), ..
            }
            | Flow::OauthMail {
                stop: Some(stop), ..
            } => stop.cancel(),
            _ => {}
        }
    }
    drop(entries);
    Ok(())
}

/// The site a drive's sign-in belongs to: where its files are read, and the
/// only place its grant may be sent.
const fn origin_of(cloud: OAuthCloud) -> &'static str {
    match cloud {
        OAuthCloud::GoogleDrive => "https://www.googleapis.com",
        OAuthCloud::OneDrive => "https://graph.microsoft.com",
        OAuthCloud::Dropbox => "https://api.dropboxapi.com",
    }
}

/// Where an `iCloud` account's session may be sent.
const ICLOUD_ORIGIN: &str = "https://www.icloud.com";

/// Keep a drive the provider's page signed in: its grant in the workspace's
/// vault, its resolved mount in its settings, and the account itself.
async fn keep_oauth_account(
    vault: VaultAt<'_>,
    cloud: OAuthCloud,
    name: &str,
    tokens: &account_credentials::TokenSet,
    root: &Path,
) -> Result<(), String> {
    let mount = CredentialStore::for_root(Some(root.to_path_buf()))
        .resolve_storage(cloud, tokens)
        .map_err(|error| format!("{}'s files were not found: {error}", cloud.label()))?;
    let id = cloud_id(cloud.type_id(), name);
    let mut config = json!({
        "id": id,
        "provider": cloud.type_id(),
        "mount": mount,
        "server_url": origin_of(cloud),
        "use_files": true,
        "use_calendar": false,
        "use_mail": false,
    });
    accounts::cloud_types::set_files_mode(&mut config, accounts::cloud_types::FilesMode::Sync);
    let secret = oauth_secret(tokens).token.unwrap_or_default();
    keep_cloud_sign_in(
        vault,
        &id,
        cloud.sign_in_with(),
        &mut config,
        Zeroizing::new(secret),
    )
    .await?;
    let mut account = accounts::Account::new(accounts::AccountId::from(id), cloud.type_id());
    name.clone_into(&mut account.display_name);
    account.config = config;
    vault.keep_account(&account).await
}

/// A provider's grant, and the store that reads what it is for.
#[derive(Clone, Copy)]
struct Grant<'a> {
    store: &'a CredentialStore,
    tokens: &'a account_credentials::TokenSet,
}

/// Keep a mailbox the provider's page signed in: its grant in the workspace's
/// vault, its account, and the servers its provider's mail lives at.
///
/// Google says which mailbox the grant is for; Microsoft's mail scopes read no
/// profile, so the address the person typed is the one kept, proved by
/// signing in at both its servers with the grant.
async fn keep_oauth_mail(
    vault: VaultAt<'_>,
    grant: Grant<'_>,
    issuer: OAuthIssuer,
    name: &str,
    typed: &str,
    replace: &str,
    services: Services,
    tester: std::sync::Arc<dyn tilo_account_probe::AccountTester>,
) -> Result<(), String> {
    let email = match issuer {
        // Asking Google which mailbox a grant is for is a blocking call, on a thread of its
        // own as every other call a person waits on here is.
        OAuthIssuer::Google => {
            let store = grant.store.clone();
            let tokens = grant.tokens.clone();
            tokio::task::spawn_blocking(move || store.gmail_identity(&tokens))
                .await
                .map_err(|_| "the mailbox was not read to the end".to_owned())?
                .map_err(|error| error.to_string())?
        }
        OAuthIssuer::Microsoft => {
            if typed.is_empty() {
                return Err(
                    "Type the address you sign in to Microsoft with, so the account is kept \
                     under it"
                        .into(),
                );
            }
            typed.to_owned()
        }
        issuer => {
            return Err(format!(
                "{} is no mailbox this server keeps",
                issuer.label()
            ));
        }
    };
    let config = issuer
        .mail_config(&email)
        .ok_or_else(|| format!("{} keeps no mailbox for {email}", issuer.label()))?;
    let id = if replace.is_empty() {
        email.clone()
    } else {
        replace.to_owned()
    };
    let mut account = accounts::Account::new(
        accounts::AccountId::from(id.clone()),
        accounts::email::TYPE_ID,
    );
    if name.trim().is_empty() {
        email.clone_into(&mut account.display_name);
    } else {
        name.trim().clone_into(&mut account.display_name);
    }
    let mut settings = config.to_value().map_err(|error| error.to_string())?;
    if let serde_json::Value::Object(settings) = &mut settings {
        settings.insert(
            "services".into(),
            serde_json::to_value(services).map_err(|error| error.to_string())?,
        );
        settings.insert("provider".into(), issuer.id().into());
    }
    account.config = settings;

    // Microsoft's address is the person's word until its servers take the grant: signing
    // in at both proves it, as a typed password is proved.
    if issuer == OAuthIssuer::Microsoft {
        let secret = accounts::Secret::password(grant.tokens.access_token.clone());
        let checking = account.clone();
        tokio::task::spawn_blocking(move || tester.test(&checking, &secret))
            .await
            .map_err(|_| "the mail server check did not finish".to_owned())??;
    }

    let before = vault
        .accounts()
        .await?
        .account(&id)
        .and_then(|before| sign_in_of(&before.config));
    let reference = vault
        .keep_sign_in(
            &id,
            (accounts::vault_items::ItemKind::Login, places_of(&account)?),
            &config.username,
            Zeroizing::new(oauth_secret(grant.tokens).token.unwrap_or_default()),
            before.as_ref(),
        )
        .await?;
    accounts::email::set_password_item(&mut account.config, Some(&reference.to_string()));
    vault.keep_account(&account).await
}

/// Finish the `iCloud` sign-in `challenge` waits on with `code`, keeping the account it signs
/// in to as `name` under `apple_id`; a code the provider turns away leaves the sign-in waiting.
async fn finish_icloud_sign_in(
    workspaces: &Workspaces,
    vault: VaultAt<'_>,
    (challenge, name, apple_id): (ICloudChallenge, String, String),
    (code, sign_in): (&str, &str),
) -> Result<(), String> {
    let auth = tokio::task::spawn_blocking({
        let workspaces = workspaces.clone();
        move || workspaces.icloud_auth()
    })
    .await
    .map_err(|_| "the verification code was not read to the end".to_owned())??;
    let outcome = tokio::task::spawn_blocking({
        let code = code.to_owned();
        move || auth.verify_code(&challenge, &code)
    })
    .await
    .map_err(|_| "the verification code was not read to the end".to_owned())?;
    let session = match outcome {
        Ok(session) => session,
        Err(error) => {
            let refused = error.to_string();
            if matches!(
                error,
                account_credentials::icloud::ICloudAuthError::InvalidCode
            ) {
                return Err(format!("{refused}. Enter the newest code"));
            }
            begun().remove(sign_in);
            return Err(refused);
        }
    };
    keep_icloud_account(vault, &name, &apple_id, &session).await?;
    begun().remove(sign_in);
    Ok(())
}

/// Keep an `Apple ID`'s session: its grant in the workspace's vault, the
/// mount Apple's own endpoints make, and the account itself.
async fn keep_icloud_account(
    vault: VaultAt<'_>,
    name: &str,
    apple_id: &str,
    session: &account_credentials::icloud::ICloudSessionState,
) -> Result<(), String> {
    let mount =
        tokio::task::spawn_blocking(|| WebICloud::apple().map(|auth| auth.storage_config()))
            .await
            .map_err(|_| "iCloud's settings were not read".to_owned())?
            .map_err(|error| error.to_string())?;
    let id = cloud_id(accounts::cloud_types::ICLOUD_DRIVE, name);
    let mut account = accounts::Account::new(
        accounts::AccountId::from(id.clone()),
        accounts::cloud_types::ICLOUD_DRIVE,
    );
    name.clone_into(&mut account.display_name);
    let mut config = json!({
        "id": id,
        "identity": apple_id,
        "provider": accounts::cloud_types::ICLOUD_DRIVE,
        "server_url": ICLOUD_ORIGIN,
        "use_files": true,
        "use_calendar": false,
        "use_mail": false,
    });
    if !mount.is_null()
        && let Some(config) = config.as_object_mut()
    {
        config.insert("mount".into(), mount);
    }
    accounts::cloud_types::set_files_mode(&mut config, accounts::cloud_types::FilesMode::Sync);
    let secret = icloud_secret(apple_id, session).token.unwrap_or_default();
    keep_cloud_sign_in(
        vault,
        &id,
        if apple_id.is_empty() {
            "iCloud"
        } else {
            apple_id
        },
        &mut config,
        Zeroizing::new(secret),
    )
    .await?;
    account.config = config;
    vault.keep_account(&account).await
}

/// Keep a `WebDAV` server's account, signed in with the password the person
/// typed, in the workspace's vault.
async fn keep_webdav_account(
    vault: VaultAt<'_>,
    name: &str,
    server: &str,
    username: &str,
    password: &Zeroizing<String>,
) -> Result<(), String> {
    let id = cloud_id(accounts::cloud_types::WEBDAV, name);
    let mut account = accounts::Account::new(
        accounts::AccountId::from(id.clone()),
        accounts::cloud_types::WEBDAV,
    );
    name.clone_into(&mut account.display_name);
    let mut config = json!({
        "id": id,
        "display_name": name,
        "server_url": server,
        "username": username,
        "provider": accounts::cloud_types::WEBDAV,
        "use_files": true,
        "use_calendar": false,
        "use_mail": false,
    });
    accounts::cloud_types::set_files_mode(&mut config, accounts::cloud_types::FilesMode::Sync);
    keep_cloud_sign_in(
        vault,
        &id,
        username,
        &mut config,
        Zeroizing::new(password.to_string()),
    )
    .await?;
    account.config = config;
    vault.keep_account(&account).await
}

/// Keep `secret` as the own sign-in of the account `id` names, at the
/// servers `config` names, and name it in `config`.
async fn keep_cloud_sign_in(
    vault: VaultAt<'_>,
    id: &str,
    username: &str,
    config: &mut Value,
    secret: Zeroizing<String>,
) -> Result<String, String> {
    let places = {
        let mut account = accounts::Account::new(
            accounts::AccountId::from(id.to_owned()),
            accounts::cloud_types::WEBDAV,
        );
        account.config = config.clone();
        places_of(&account)?
    };
    let reference = vault
        .keep_sign_in(
            id,
            (accounts::vault_items::ItemKind::Login, places),
            username,
            secret,
            None,
        )
        .await?;
    let reference = reference.to_string();
    accounts::email::set_password_item(config, Some(&reference));
    Ok(reference)
}

/// The sign-in a mail account reads and sends with, for `by`: its password, read from the
/// entry of the workspace's vault the account names, at the account's two servers.
pub(super) async fn mail_secret(
    vault: VaultAt<'_>,
    root: &Path,
    account: &accounts::Account,
    by: &str,
) -> Result<accounts::Secret, String> {
    let reference =
        sign_in_of(&account.config).ok_or_else(|| super::kept_sign_ins::NO_SIGN_IN.to_owned())?;
    let places = places_of(account)?;
    let released = vault
        .use_sign_in(
            &reference,
            (accounts::vault_items::ItemKind::Login, &places),
            by,
            SIGN_IN,
        )
        .await?;
    // An account a provider signed in on its own page reads with the token its grant holds
    // now, renewed here; every other account signs in with the password it keeps.
    let released = super::oauth_grant::signs_in_with(vault, root, account, released).await?;
    Ok(accounts::Secret::password(released.secret.to_string()))
}

/// Keep the Nextcloud account a finished login flow made, its app password in
/// the vault, and the calendar server of the same sign-in when the person
/// asked for calendars.
async fn keep_nextcloud(
    vault: VaultAt<'_>,
    login: &nextcloud::NextcloudLogin,
    services: Services,
    replace: &str,
) -> Result<(), String> {
    let server = login.server.trim().trim_end_matches('/').to_owned();
    let Ok(url) = Url::parse(&server) else {
        return Err("the server's address does not read".into());
    };
    let site = match url.domain() {
        Some(domain) => domain.to_owned(),
        None => named_by(&url)?,
    };
    let existing = vault
        .accounts()
        .await?
        .of_type(accounts::cloud_types::NEXTCLOUD)
        .into_iter()
        .find(|account| {
            account.type_id.as_str() == accounts::cloud_types::NEXTCLOUD
                && account.config.get("server_url").and_then(Value::as_str) == Some(server.as_str())
                && account.config.get("username").and_then(Value::as_str)
                    == Some(login.login_name.as_str())
        })
        .map(|account| account.id.to_string());
    let id = if replace.trim().is_empty() {
        existing.unwrap_or_else(|| format!("nextcloud:{}@{site}", login.login_name))
    } else {
        replace.trim().to_owned()
    };
    let mut kept = accounts::Account::new(
        accounts::AccountId::from(id.clone()),
        accounts::cloud_types::NEXTCLOUD,
    );
    site.clone_into(&mut kept.display_name);
    kept.config = json!({
        "id": id,
        "server_url": server,
        "username": login.login_name.clone(),
        "provider": "nextcloud",
        "use_files": services.files,
        "use_calendar": services.calendar,
        "use_mail": false,
        "services": services,
    });
    keep_app_password(vault, &mut kept, login).await?;
    if services.calendar {
        keep_nextcloud_calendar(vault, &server, &site, login).await?;
    }
    Ok(())
}

/// Keep the app password `login` minted as `kept`'s own sign-in, in place of the one it named
/// before, and the account with it.
async fn keep_app_password(
    vault: VaultAt<'_>,
    kept: &mut accounts::Account,
    login: &nextcloud::NextcloudLogin,
) -> Result<(), String> {
    let before = vault
        .accounts()
        .await?
        .account(kept.id.as_str())
        .and_then(|before| sign_in_of(&before.config));
    let reference = vault
        .keep_sign_in(
            kept.id.as_str(),
            (accounts::vault_items::ItemKind::Login, places_of(kept)?),
            &login.login_name,
            zeroize::Zeroizing::new(login.app_password.to_string()),
            before.as_ref(),
        )
        .await?;
    accounts::email::set_password_item(&mut kept.config, Some(&reference.to_string()));
    vault.keep_account(kept).await
}

/// Keep the `CalDAV` account of a Nextcloud server at its own DAV address,
/// signed in with the app password the flow minted.
async fn keep_nextcloud_calendar(
    vault: VaultAt<'_>,
    server: &str,
    site: &str,
    login: &nextcloud::NextcloudLogin,
) -> Result<(), String> {
    let server_url = format!("{server}/remote.php/dav");
    let id = format!("caldav:{}@{site}", login.login_name);
    let mut kept = accounts::Account::new(accounts::AccountId::from(id), CALDAV_TYPE);
    login.login_name.clone_into(&mut kept.display_name);
    kept.config = json!({
        "display_name": login.login_name.clone(),
        "server_url": server_url,
        "username": login.login_name.clone(),
        "calendar_provider": "nextcloud",
    });
    keep_app_password(vault, &mut kept, login).await
}

/// The name a server's address answers to, without its scheme, port or
/// path: what a reach judges.
fn named_by(url: &Url) -> Result<String, String> {
    if let Some(address) = tilo_net::address::written_out(url) {
        return Ok(address.to_string());
    }
    url.domain()
        .map(str::to_owned)
        .ok_or_else(|| "The server's address does not read".into())
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use test_http::CountingListener;
    use tilo_account_probe::imap::Reach;

    use super::*;
    use crate::workspaces::tests::{hosting_device, test_host};

    /// A look-up that answers with a public address on its first asking and
    /// `127.0.0.1` afterwards, counting every call.
    fn turning(asked: &Arc<AtomicUsize>) -> tilo_net::address::Lookup {
        let asked = Arc::clone(asked);
        Arc::new(move |_name: String| {
            let nth = asked.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                Ok(vec![if nth == 0 {
                    IpAddr::from([93, 184, 216, 34])
                } else {
                    IpAddr::from(Ipv4Addr::LOCALHOST)
                }])
            })
        })
    }

    fn nextcloud_asking(server: String) -> BeginSignIn {
        BeginSignIn {
            provider: Provider::Nextcloud,
            server,
            mail: true,
            calendar: false,
            files: false,
            name: String::new(),
            address: String::new(),
            account: String::new(),
        }
    }

    #[test]
    fn named_by_reads_parsed_hosts_and_judges_written_out_addresses() {
        let public = Reach::public();
        for (input, expected) in [
            ("https://[2606:4700:4700::1111]", "2606:4700:4700::1111"),
            (
                "https://[2606:4700:4700::1111]:8443/nextcloud",
                "2606:4700:4700::1111",
            ),
        ] {
            let url = Url::parse(input).unwrap();
            let named = named_by(&url).unwrap();
            assert_eq!(named, expected, "{input}");
            assert!(
                public.takes(&named),
                "public IPv6 literal {input} is judged public"
            );
        }

        for (input, expected) in [
            ("https://[::1]", "::1"),
            ("https://10.0.0.7", "10.0.0.7"),
            ("https://2130706433", "127.0.0.1"),
        ] {
            let url = Url::parse(input).unwrap();
            let named = named_by(&url).unwrap();
            assert_eq!(named, expected, "{input}");
            assert!(
                !public.takes(&named),
                "inside address {input} is refused under public-only reach"
            );
        }

        for (input, expected) in [
            ("https://cloud.example.test/nextcloud", "cloud.example.test"),
            (
                "https://cloud.example.test:8443/nextcloud",
                "cloud.example.test",
            ),
        ] {
            let url = Url::parse(input).unwrap();
            assert_eq!(named_by(&url).unwrap(), expected, "{input}");
        }

        assert_eq!(
            named_by(&Url::parse("mailto:ada@example.test").unwrap()),
            Err("The server's address does not read".into())
        );
    }

    #[tokio::test]
    async fn a_nextcloud_sign_in_whose_name_turns_inside_connects_to_nobody() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = hosting_device();
        let host = test_host(dir.path(), &key, &owner).await;
        let workspaces = host.workspaces();
        let listener = CountingListener::start();
        let server = format!("https://localhost:{}", listener.port());
        let asking = nextcloud_asking(server);

        let asked = Arc::new(AtomicUsize::new(0));
        let public = Reach::public().looked_up_by(turning(&asked));
        let refused = workspaces
            .begin_sign_in(([1; 32], "workspace-a", None), &asking, &public)
            .await
            .unwrap_err();
        assert_eq!(
            refused,
            "could not reach the server: localhost answers with an address of this machine or its network"
        );
        assert_eq!(listener.count(), 0, "nothing is connected to");
        assert_eq!(
            asked.load(Ordering::SeqCst),
            2,
            "judged once before sending and once at the connection"
        );

        // Inside written-out literals are refused before anything is sent.
        let strict = Reach::public();
        for inside in [
            "https://[::1]",
            "https://10.0.0.7",
            "https://2130706433",
            &format!("http://127.0.0.1:{}", listener.port()),
        ] {
            let err = workspaces
                .begin_sign_in(
                    ([1; 32], "workspace-a", None),
                    &nextcloud_asking(inside.to_owned()),
                    &strict,
                )
                .await
                .unwrap_err();
            assert_eq!(
                err, "That server's address is not one this server may call",
                "{inside}"
            );
        }
        assert_eq!(listener.count(), 0, "inside literals reach nobody");

        // An inside name is refused before anything is sent, and a name that
        // does not resolve gets the not-found answer rather than a refusal;
        // the listener is reached in neither case.
        let inside_lookup: tilo_net::address::Lookup = Arc::new(|_name: String| {
            Box::pin(async { Ok(vec![IpAddr::from(Ipv4Addr::LOCALHOST)]) })
        });
        let inside_reach = Reach::public().looked_up_by(inside_lookup);
        let inside_named = format!("https://inside.example.test:{}", listener.port());
        assert_eq!(
            workspaces
                .begin_sign_in(
                    ([1; 32], "workspace-a", None),
                    &nextcloud_asking(inside_named),
                    &inside_reach,
                )
                .await
                .unwrap_err(),
            "That server's address is not one this server may call"
        );

        let missing_lookup: tilo_net::address::Lookup = Arc::new(|name: String| {
            Box::pin(async move {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("{name} does not resolve"),
                ))
            })
        });
        let empty_lookup: tilo_net::address::Lookup =
            Arc::new(|_name: String| Box::pin(async { Ok(Vec::new()) }));
        let missing_named = format!("https://missing.example.test:{}", listener.port());
        for unresolvable in [
            Reach::public().looked_up_by(missing_lookup),
            Reach::public().looked_up_by(empty_lookup),
        ] {
            assert_eq!(
                workspaces
                    .begin_sign_in(
                        ([1; 32], "workspace-a", None),
                        &nextcloud_asking(missing_named.clone()),
                        &unresolvable,
                    )
                    .await
                    .unwrap_err(),
                "No server was found under that name"
            );
        }
        assert_eq!(listener.count(), 0, "inside and missing names reach nobody");

        // Where the operator lets the server call its own network, it connects
        // as on main, proving the listener counts connections.
        let open = workspaces
            .begin_sign_in(([1; 32], "workspace-a", None), &asking, &Reach::any())
            .await;
        assert!(
            open.is_err(),
            "a closing listener is not a Nextcloud server"
        );
        assert_eq!(
            listener.count(),
            1,
            "an open reach connects to the server's own network"
        );
    }
}
