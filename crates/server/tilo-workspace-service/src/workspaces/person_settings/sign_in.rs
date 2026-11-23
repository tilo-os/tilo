//! Signing in on a provider's own page: a Nextcloud server's login flow.
//!
//! The server starts the flow ([`BEGIN_SIGN_IN`]), shows its page, and polls
//! in the background until the person approved access there or the flow ran
//! out. The app password it mints is kept in the workspace's vault as the
//! account's own entry ([`super::kept_sign_ins`]).
//!
//! What is begun is kept in memory alone, one entry per sign-in, expiring;
//! no app password is ever answered to a caller.
//!
//! [`BEGIN_SIGN_IN`]: tilo_actions::remote::person_accounts::BEGIN_SIGN_IN

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, mpsc};
use std::time::Duration;

use account_credentials::nextcloud;
use serde_json::{Value, json};
use tilo_actions::remote::person_accounts::{
    BeginSignIn, CancelSignIn, Provider, Services, SignInStep, SigningIn,
};
use tilo_net::Url;

use super::super::vault::VaultAt;
use super::kept_sign_ins::{SIGN_IN, places_of, sign_in_of};
use crate::workspaces::Workspaces;

/// How long a Nextcloud login flow may stay open.
const NEXTCLOUD_MINUTES: u64 = 20;
/// How often a Nextcloud server is asked whether the person approved.
const POLL_EVERY: Duration = Duration::from_secs(2);
/// The most sign-ins begun and not forgotten at once.
const MAX_BEGUN: usize = 64;
/// The account type a calendar server is kept as, in the workspace's accounts.
const CALDAV_TYPE: &str = "caldav";

/// A Nextcloud login flow begun and not finished, polled in the background.
struct Begun {
    id: String,
    person: [u8; 32],
    /// Where the workspace this sign-in keeps its account in keeps what is its own.
    root: PathBuf,
    until_ms: u64,
    failed: Option<String>,
    /// The Nextcloud server it signs in to.
    server: String,
    /// Held by the poll; dropping it stops it.
    stop: Option<mpsc::Sender<()>>,
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

/// The providers this server can sign a person in with on their own page:
/// Nextcloud's login flow.
#[must_use]
pub(super) fn signs_in_with() -> Vec<Provider> {
    vec![Provider::Nextcloud]
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
        .map(|entry| SigningIn {
            id: entry.id.clone(),
            provider: Provider::Nextcloud,
            server: entry.server.clone(),
            failed: entry.failed.clone(),
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
        (person, workspace): ([u8; 32], &str),
        asking: &BeginSignIn,
        reach: &tilo_account_probe::imap::Reach,
    ) -> Result<SignInStep, String> {
        let provider = asking.provider;
        let services = chosen(provider, asking.services);
        prune();
        if begun().len() >= MAX_BEGUN {
            return Err("Too many sign-ins are open here. Finish or cancel one first".into());
        }
        if provider == Provider::Nextcloud {
            self.begin_nextcloud((person, workspace), services, asking, reach)
                .await
        } else {
            Err(format!(
                "{} signs in with a password, not on its own page",
                provider_label(provider)
            ))
        }
    }

    async fn begin_nextcloud(
        &self,
        (person, workspace): ([u8; 32], &str),
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
        let flow = tokio::task::spawn_blocking({
            let server = server.clone();
            let reach = reach.clone();
            move || {
                if !reach.takes(&named) {
                    return Err("That server's address is not one this server may call".into());
                }
                nextcloud::begin_looking(&server, reach.lookup()).map_err(|error| error.to_string())
            }
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
                server,
                stop: Some(stop),
            },
        );
        let workspaces = self.clone();
        let workspace = workspace.to_owned();
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
                Ok(Ok(login)) => {
                    Ok(
                        keep_nextcloud(workspaces.vault_at(&workspace), &login, services, &account)
                            .await,
                    )
                }
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
        Ok(SignInStep { id, open })
    }

    /// Begin the sign-in a request asks for, and wake the windows following
    /// the accounts once it is listed.
    pub(super) async fn begin_account_sign_in(
        &self,
        (person, workspace): ([u8; 32], &str),
        asking: &BeginSignIn,
    ) -> Result<SignInStep, String> {
        let reach = self.inner.models.mail_reach();
        let step = self
            .begin_sign_in((person, workspace), asking, &reach)
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
}

/// Keep a sign-in's failure with it, so the person reads why and can cancel
/// it, and stop a Nextcloud poll that is still running.
fn keep_failed(id: &str, reason: &str, until_ms: u64) {
    let mut entries = begun();
    if let Some(entry) = entries.get_mut(id) {
        entry.failed = Some(reason.to_owned());
        entry.until_ms = until_ms;
        entry.stop = None;
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
    use tilo_actions::remote::person_accounts::{BEGIN_SIGN_IN, CANCEL_SIGN_IN};

    match request.target.address.action.as_str() {
        BEGIN_SIGN_IN => {
            let asking: BeginSignIn = super::argument(request, "a provider to sign in with")?;
            super::answer(
                &workspaces
                    .begin_account_sign_in((person, workspace), &asking)
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
    let theirs = begun()
        .get(&cancel.sign_in)
        .is_some_and(|entry| &entry.person == person);
    if !theirs {
        return Err("That sign-in was not begun here".into());
    }
    begun().remove(&cancel.sign_in);
    Ok(())
}

/// The sign-in a mail account reads and sends with, for `by`: its password, read from the
/// entry of the workspace's vault the account names, at the account's two servers.
pub(super) async fn mail_secret(
    vault: VaultAt<'_>,
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
        "use_files": false,
        "use_calendar": services.calendar,
        "use_mail": false,
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
    use std::net::{IpAddr, Ipv4Addr, TcpListener, TcpStream};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use tilo_account_probe::imap::Reach;

    use super::*;
    use crate::workspaces::tests::{governor, test_host};

    /// A loopback TCP listener that counts every accepted connection and closes
    /// it immediately, so a client never waits out a read timeout.
    struct CountingListener {
        port: u16,
        accepted: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
        worker: Option<std::thread::JoinHandle<()>>,
    }

    impl CountingListener {
        fn start() -> Self {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("a port here");
            let port = listener.local_addr().expect("its address").port();
            let accepted = Arc::new(AtomicUsize::new(0));
            let stop = Arc::new(AtomicBool::new(false));
            let count = Arc::clone(&accepted);
            let stopping = Arc::clone(&stop);
            let worker = std::thread::spawn(move || {
                while let Ok((stream, _)) = listener.accept() {
                    if stopping.load(Ordering::SeqCst) {
                        break;
                    }
                    count.fetch_add(1, Ordering::SeqCst);
                    drop(stream);
                }
            });
            Self {
                port,
                accepted,
                stop,
                worker: Some(worker),
            }
        }

        fn count(&self) -> usize {
            self.accepted.load(Ordering::SeqCst)
        }
    }

    impl Drop for CountingListener {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            let _ = TcpStream::connect((Ipv4Addr::LOCALHOST, self.port));
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

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
            services: Services::default(),
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
        let (key, owner) = governor();
        let host = test_host(dir.path(), &key, &owner).await;
        let workspaces = host.workspaces();
        let listener = CountingListener::start();
        let server = format!("https://localhost:{}", listener.port);
        let asking = nextcloud_asking(server);

        let asked = Arc::new(AtomicUsize::new(0));
        let public = Reach::public().looked_up_by(turning(&asked));
        let refused = workspaces
            .begin_sign_in(([1; 32], "workspace-a"), &asking, &public)
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
            &format!("http://127.0.0.1:{}", listener.port),
        ] {
            let err = workspaces
                .begin_sign_in(
                    ([1; 32], "workspace-a"),
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

        // Where the operator lets the server call its own network, it connects
        // as on main, proving the listener counts connections.
        let open = workspaces
            .begin_sign_in(([1; 32], "workspace-a"), &asking, &Reach::any())
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
