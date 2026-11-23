//! The connections an account's sign-in makes to the account's own servers, made by the session
//! for Settings.
//!
//! Settings collects an account's fields, but its sandbox has no network, and turning the
//! network on for all of Settings to reach one server would hand every pane of it the internet.
//! So the session, which has the network, makes exactly the connections adding an account needs:
//! the launcher hands Settings alone a connection to the session's `accounts` service
//! (`launcher::services::ACCOUNTS_SERVICE`), and what Settings asks on it is typed: sign in to
//! this account's servers, list this account's calendars, or start and follow a Nextcloud
//! sign-in at this server. Nothing on it opens an arbitrary connection or carries one's bytes:
//! each answer is the outcome, in words a person can act on.
//!
//! Every message is one JSON line. One request is answered before the next is read; Settings
//! asks from its worker threads, which take turns on the one connection ([`Connection`]).

use std::io::{BufRead, BufReader, ErrorKind, Read as _, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::AccountTester;

/// The version of what crosses the connection; Settings and the session speak only the same one
/// (`interface.toml`).
pub const PROTOCOL: u16 = 1;

/// The name the launcher hands the service under, and the session serves it at.
pub const SERVICE: &str = "accounts";

/// The variable the launcher names the handed connection's descriptor in (it uppercases the
/// service's name).
pub const FD_ENV: &str = "TILO_SVC_ACCOUNTS_FD";

/// The longest line either side writes: an account and its secret, or a list of calendars.
pub const MAX_LINE_BYTES: usize = 256 * 1024;

/// How long one answer may take: a sign-in reaches two servers, each within the probe's own
/// timeouts, and a calendar discovery follows a few redirects.
const ANSWER_WAIT: Duration = Duration::from_secs(90);

/// What Settings asks the session to connect for.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "ask", rename_all = "snake_case")]
pub enum Request {
    /// Sign in to the servers `account` names with `secret`, and leave.
    Test {
        account: accounts::Account,
        secret: accounts::Secret,
    },
    /// The calendars of the `CalDAV` account, as its server lists them.
    DiscoverCalendars {
        account: accounts::Account,
        secret: accounts::Secret,
    },
    /// Start a Nextcloud sign-in at `server`: the answer is the page the person approves it on.
    /// One sign-in is followed per connection; starting another ends the one before.
    NextcloudBegin { server: String },
    /// Whether the person approved the sign-in on its page yet.
    NextcloudPoll,
    /// Stop following the sign-in.
    NextcloudEnd,
    /// Start signing in to the cloud storage `provider` (an account type id: `google-drive`,
    /// `onedrive`, `dropbox` or `icloud-drive`) in a browser the session opens: the answer is
    /// the page the person approves it on, or that a code is needed. One sign-in is followed
    /// per connection; starting another ends the one before.
    CloudSignIn {
        provider: String,
        /// The account's name, as the person reads it.
        label: String,
        /// The Apple ID to sign in with (`icloud-drive` only).
        #[serde(default)]
        apple_id: Option<String>,
        /// The Apple ID's password, used once and never kept (`icloud-drive` only).
        #[serde(default)]
        password: Option<String>,
        /// The address to start an `OAuth` sign-in at.
        #[serde(default)]
        login_hint: Option<String>,
    },
    /// Answer Apple's verification code, or the code the provider showed because no browser
    /// can be redirected back to this device.
    CloudCode { code: String },
    /// Whether the cloud sign-in finished yet.
    CloudPoll,
    /// Stop following the cloud sign-in.
    CloudEnd,
}

/// One calendar of an account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Calendar {
    pub href: String,
    pub name: String,
    pub read_only: bool,
}

/// What the session answers.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case")]
pub enum Reply {
    /// The sign-in was accepted, as the probe says it.
    Tested {
        said: String,
    },
    Calendars {
        calendars: Vec<Calendar>,
    },
    /// The page the person approves the Nextcloud sign-in on. `shown` when the session opened
    /// it in a window of its own, which it closes once the sign-in is over; else Settings opens
    /// it.
    NextcloudPage {
        url: String,
        #[serde(default)]
        shown: bool,
    },
    /// The person has not approved it yet.
    NextcloudWaiting,
    /// The person approved it: the server minted this device its own app password.
    NextcloudSignedIn {
        server: String,
        login_name: String,
        app_password: String,
    },
    /// The page the person approves the cloud sign-in on; `shown` as for a Nextcloud page.
    CloudPage {
        url: String,
        #[serde(default)]
        shown: bool,
    },
    /// The session needs a code before it can finish: Apple's verification code, or the code a
    /// provider shows when no browser can be redirected back to this device.
    CloudCode {
        message: String,
    },
    /// The person has not finished approving the sign-in yet.
    CloudWaiting,
    /// The person signed in: the account it makes, and the secret it keeps.
    CloudSignedIn {
        account: accounts::Account,
        secret: accounts::Secret,
    },
    /// What went wrong, in words a person can act on.
    Failed {
        message: String,
    },
}

/// Where a sign-in's page shows: a window the system opens for it alone and closes once the
/// sign-in it was for is over, so the person never closes a page that did its job, and a page
/// they were reading is never navigated away to sign in.
pub trait SignInPages {
    /// Show `url` in a window of its own: the page's id, or `None` when nothing here can show it
    /// (Settings then opens it itself).
    fn show(&self, url: &str) -> Option<u64>;
    /// The sign-in page `page` was shown for is over: signed in, failed, cancelled or left.
    fn over(&self, page: u64);
}

/// No window of its own for a sign-in's page: Settings opens it.
pub struct NoSignInPages;

impl SignInPages for NoSignInPages {
    fn show(&self, _url: &str) -> Option<u64> {
        None
    }

    fn over(&self, _page: u64) {}
}

/// A Nextcloud sign-in this connection follows, and the page shown for it.
struct Following<'a> {
    flow: Option<account_credentials::nextcloud::LoginFlow>,
    page: Option<u64>,
    pages: &'a dyn SignInPages,
}

impl Following<'_> {
    /// The sign-in is over: its page goes.
    fn end(&mut self) {
        self.flow = None;
        if let Some(page) = self.page.take() {
            self.pages.over(page);
        }
    }
}

impl Drop for Following<'_> {
    fn drop(&mut self) {
        self.end();
    }
}

/// Where a cloud sign-in stands on this connection.
enum CloudStage {
    /// The person approves the sign-in in their browser; the tokens come back from the
    /// exchange running on its own thread.
    Oauth {
        pending: std::sync::mpsc::Receiver<Result<(accounts::Account, accounts::Secret), String>>,
        page: Option<u64>,
        /// Stops the loopback redirect listener while nothing has come back.
        canceller: Option<account_credentials::LoopbackCancel>,
    },
    /// The provider shows the code instead of redirecting back: the person pastes it.
    Pasted {
        store: account_credentials::CredentialStore,
        cloud: account_credentials::OAuthCloud,
        label: String,
        authorization: Box<account_credentials::Authorization>,
    },
    /// Apple sent a verification code to the person's trusted devices.
    Icloud {
        auth: Arc<dyn account_credentials::icloud::ICloudAuth>,
        apple_id: String,
        challenge: account_credentials::icloud::ICloudChallenge,
    },
}

/// A cloud storage sign-in this connection follows, and the page shown for it.
struct CloudFollowing<'a> {
    stage: Option<CloudStage>,
    pages: &'a dyn SignInPages,
}

impl CloudFollowing<'_> {
    fn end(&mut self) {
        match self.stage.take() {
            Some(CloudStage::Oauth {
                page, canceller, ..
            }) => {
                if let Some(canceller) = canceller {
                    canceller.cancel();
                }
                if let Some(page) = page {
                    self.pages.over(page);
                }
            }
            Some(CloudStage::Pasted { .. } | CloudStage::Icloud { .. }) | None => {}
        }
    }

    fn begin(
        &mut self,
        provider: &str,
        label: &str,
        apple_id: Option<String>,
        password: Option<String>,
        login_hint: Option<String>,
    ) -> Reply {
        self.end();
        let store = account_credentials::CredentialStore::live().with_apple_icloud();
        if provider == accounts::cloud_types::ICLOUD_DRIVE {
            let auth = Arc::clone(store.icloud());
            if !auth.is_available() {
                return Reply::Failed {
                    message: account_credentials::icloud::ICloudAuthError::Unavailable.to_string(),
                };
            }
            let Some(apple_id) = apple_id.filter(|id| !id.trim().is_empty()) else {
                return Reply::Failed {
                    message: "enter your Apple ID".into(),
                };
            };
            let Some(password) = password.filter(|password| !password.is_empty()) else {
                return Reply::Failed {
                    message: "enter your Apple ID's password".into(),
                };
            };
            use account_credentials::icloud::ICloudSignIn;
            return match auth.sign_in(&apple_id, &password) {
                Ok(ICloudSignIn::Complete(session)) => {
                    cloud_account(store.icloud_account(&apple_id, &session))
                }
                Ok(ICloudSignIn::NeedsCode(challenge)) => {
                    self.stage = Some(CloudStage::Icloud {
                        auth,
                        apple_id,
                        challenge,
                    });
                    Reply::CloudCode {
                        message: "Apple sent a verification code to your trusted devices. Enter it here to continue."
                            .into(),
                    }
                }
                Err(error) => Reply::Failed {
                    message: error.to_string(),
                },
            };
        }
        let Some(cloud) = account_credentials::OAuthCloud::from_type_id(provider) else {
            return Reply::Failed {
                message: "this account type does not sign in on its provider's page".into(),
            };
        };
        let authorization = match store.begin_sign_in(cloud, login_hint.as_deref()) {
            Ok(authorization) => authorization,
            Err(error) => {
                return Reply::Failed {
                    message: error.to_string(),
                };
            }
        };
        let url = authorization.url().to_owned();
        let Some(canceller) = authorization.canceller() else {
            self.stage = Some(CloudStage::Pasted {
                store,
                cloud,
                label: label.to_owned(),
                authorization: Box::new(authorization),
            });
            return Reply::CloudPage { url, shown: false };
        };
        let page = self.pages.show(&url);
        let (sender, pending) = std::sync::mpsc::channel();
        let thread_store = store.clone();
        let thread_label = label.to_owned();
        std::thread::spawn(move || {
            let outcome = account_credentials::CredentialStore::finish_sign_in(authorization)
                .and_then(|tokens| thread_store.cloud_account(cloud, &thread_label, &tokens))
                .map_err(|error| error.to_string());
            let _ = sender.send(outcome);
        });
        self.stage = Some(CloudStage::Oauth {
            pending,
            page,
            canceller: Some(canceller),
        });
        Reply::CloudPage {
            url,
            shown: page.is_some(),
        }
    }

    fn poll(&mut self) -> Reply {
        match &mut self.stage {
            None => Reply::Failed {
                message: "no cloud sign-in was started".into(),
            },
            Some(CloudStage::Oauth { pending, .. }) => match pending.try_recv() {
                Ok(Ok((account, secret))) => {
                    self.end();
                    Reply::CloudSignedIn { account, secret }
                }
                Ok(Err(message)) => {
                    self.end();
                    Reply::Failed { message }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => Reply::CloudWaiting,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.end();
                    Reply::Failed {
                        message: "the sign-in stopped unexpectedly".into(),
                    }
                }
            },
            Some(CloudStage::Pasted { .. } | CloudStage::Icloud { .. }) => Reply::CloudWaiting,
        }
    }

    fn code(&mut self, code: &str) -> Reply {
        let code = code.trim().to_owned();
        if code.is_empty() {
            return Reply::Failed {
                message: "enter the code you were shown".into(),
            };
        }
        let Some(stage) = self.stage.take() else {
            return Reply::Failed {
                message: "no cloud sign-in was started".into(),
            };
        };
        match stage {
            CloudStage::Pasted {
                store,
                cloud,
                label,
                authorization,
            } => {
                let finished = account_credentials::CredentialStore::finish_sign_in_with_code(
                    *authorization,
                    &code,
                )
                .and_then(|tokens| store.cloud_account(cloud, &label, &tokens));
                cloud_account(finished)
            }
            CloudStage::Icloud {
                auth,
                apple_id,
                challenge,
            } => {
                use account_credentials::icloud::ICloudAuthError;
                let verified = auth.verify_code(&challenge, &code);
                match verified {
                    Ok(session) => {
                        let store =
                            account_credentials::CredentialStore::live().with_apple_icloud();
                        cloud_account(store.icloud_account(&apple_id, &session))
                    }
                    Err(ICloudAuthError::InvalidCode) => {
                        // The newest code can be tried without signing in again.
                        self.stage = Some(CloudStage::Icloud {
                            auth,
                            apple_id,
                            challenge,
                        });
                        Reply::Failed {
                            message: ICloudAuthError::InvalidCode.to_string(),
                        }
                    }
                    Err(error) => Reply::Failed {
                        message: error.to_string(),
                    },
                }
            }
            other => {
                let page = match &other {
                    CloudStage::Oauth { page, .. } => *page,
                    CloudStage::Pasted { .. } | CloudStage::Icloud { .. } => None,
                };
                if let Some(page) = page {
                    self.pages.over(page);
                }
                Reply::Failed {
                    message: "this sign-in needs no code".into(),
                }
            }
        }
    }
}

fn cloud_account(
    made: Result<(accounts::Account, accounts::Secret), account_credentials::CredentialError>,
) -> Reply {
    match made {
        Ok((account, secret)) => Reply::CloudSignedIn { account, secret },
        Err(error) => Reply::Failed {
            message: error.to_string(),
        },
    }
}

/// Serve one connection until it closes: each request answered with what the account's servers
/// said, through `tester` and the Nextcloud login flow, whose page `pages` shows.
pub fn serve(stream: UnixStream, tester: &dyn AccountTester, pages: &dyn SignInPages) {
    let Ok(mut writing) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let mut nextcloud = Following {
        flow: None,
        page: None,
        pages,
    };
    let mut cloud = CloudFollowing { stage: None, pages };
    loop {
        let reply = match read_line(&mut reader) {
            Ok(Some(line)) => match serde_json::from_slice::<Request>(&line) {
                Ok(request) => answer(request, tester, &mut nextcloud, &mut cloud),
                Err(_) => Reply::Failed {
                    message: "Settings asked something the session does not understand".into(),
                },
            },
            // The connection closed, or what it sent cannot be read: nothing more is asked.
            Ok(None) | Err(_) => return,
        };
        if write_line(&mut writing, &reply).is_err() {
            return;
        }
    }
}

fn answer(
    request: Request,
    tester: &dyn AccountTester,
    nextcloud: &mut Following<'_>,
    cloud: &mut CloudFollowing<'_>,
) -> Reply {
    use account_credentials::nextcloud;
    let failed = |message: String| Reply::Failed { message };
    match request {
        Request::Test { account, secret } => tester
            .test(&account, &secret)
            .map_or_else(failed, |said| Reply::Tested { said }),
        Request::DiscoverCalendars { account, secret } => tester
            .discover_calendars(&account, &secret)
            .map_or_else(failed, |found| Reply::Calendars {
                calendars: found
                    .into_iter()
                    .map(|calendar| Calendar {
                        href: calendar.href,
                        name: calendar.name,
                        read_only: calendar.read_only,
                    })
                    .collect(),
            }),
        Request::NextcloudBegin { server } => {
            nextcloud.end();
            match nextcloud::begin(&server) {
                Ok(flow) => {
                    let url = flow.login_url().to_owned();
                    nextcloud.flow = Some(flow);
                    nextcloud.page = nextcloud.pages.show(&url);
                    Reply::NextcloudPage {
                        url,
                        shown: nextcloud.page.is_some(),
                    }
                }
                Err(error) => failed(error.to_string()),
            }
        }
        Request::NextcloudPoll => match nextcloud.flow.as_ref().map(nextcloud::LoginFlow::poll) {
            None => failed("no Nextcloud sign-in was started".into()),
            Some(Ok(None)) => Reply::NextcloudWaiting,
            Some(Ok(Some(login))) => {
                nextcloud.end();
                Reply::NextcloudSignedIn {
                    server: login.server,
                    login_name: login.login_name,
                    app_password: login.app_password.as_str().to_owned(),
                }
            }
            Some(Err(error)) => {
                nextcloud.end();
                failed(error.to_string())
            }
        },
        Request::NextcloudEnd => {
            nextcloud.end();
            Reply::NextcloudWaiting
        }
        Request::CloudSignIn {
            provider,
            label,
            apple_id,
            password,
            login_hint,
        } => cloud.begin(&provider, &label, apple_id, password, login_hint),
        Request::CloudCode { code } => cloud.code(&code),
        Request::CloudPoll => cloud.poll(),
        Request::CloudEnd => {
            cloud.end();
            Reply::CloudWaiting
        }
    }
}

/// Settings' end of the service: its requests, one at a time, on the connection the launcher
/// handed it.
pub struct Connection {
    stream: Mutex<BufReader<UnixStream>>,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection").finish_non_exhaustive()
    }
}

/// A Nextcloud sign-in the person approved.
pub struct NextcloudSignedIn {
    pub server: String,
    pub login_name: String,
    pub app_password: Zeroizing<String>,
}

/// A cloud storage sign-in the person approved: the account it makes and the secret it keeps.
pub struct CloudSignedIn {
    pub account: accounts::Account,
    pub secret: accounts::Secret,
}

/// Where a cloud sign-in stands, as Settings follows it.
pub enum CloudProgress {
    /// The page the person approves it on, and whether the session shows it itself.
    Page {
        url: String,
        shown: bool,
    },
    /// The session needs a code: Apple's, or the provider's when no browser can come back.
    Code {
        message: String,
    },
    /// The person has not finished approving it yet.
    Waiting,
    SignedIn(CloudSignedIn),
}

impl Connection {
    /// The connection the launcher handed this program, once: `None` where it was handed none
    /// (an app other than Settings, or one started outside a session), or after the first call
    /// took it.
    #[must_use]
    pub fn handed() -> Option<Self> {
        use std::os::fd::{FromRawFd as _, RawFd};
        static TAKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let fd = std::env::var(FD_ENV).ok()?.parse::<RawFd>().ok()?;
        // SAFETY: `fcntl(F_GETFD)` only reads the descriptor's flags; a closed slot answers -1.
        if fd < 3 || unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1 {
            return None;
        }
        if TAKEN.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return None;
        }
        // SAFETY: the launcher handed this descriptor for this service alone, it is open, and the
        // flag above lets only the first caller own it.
        let stream = unsafe { UnixStream::from_raw_fd(fd) };
        Self::new(stream).ok()
    }

    /// Speak on `stream`.
    ///
    /// # Errors
    /// The stream's timeout cannot be set.
    pub fn new(stream: UnixStream) -> std::io::Result<Self> {
        stream.set_read_timeout(Some(ANSWER_WAIT))?;
        stream.set_write_timeout(Some(ANSWER_WAIT))?;
        Ok(Self {
            stream: Mutex::new(BufReader::new(stream)),
        })
    }

    /// Ask `request` and read its answer. A connection that failed says so in words a person can
    /// act on.
    fn ask(&self, request: &Request) -> Result<Reply, String> {
        let lost = |_| {
            "the session stopped answering Settings' connection to the account's servers: \
             restart Settings and try again"
                .to_owned()
        };
        let mut stream = self
            .stream
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        write_line(stream.get_mut(), request).map_err(lost)?;
        let line = read_line(&mut stream).map_err(lost)?.ok_or_else(|| {
            "the session closed Settings' connection to the account's servers: restart Settings \
             and try again"
                .to_owned()
        })?;
        drop(stream);
        serde_json::from_slice(&line)
            .map_err(|_| "the session answered something Settings does not understand".to_owned())
    }

    /// Sign in at the Nextcloud server `server`: `page` is handed the page the person approves it
    /// on, and whether the session already shows it in a window of its own (which it closes when
    /// the sign-in is over), and the sign-in is followed until they did, `cancel` is sent on or
    /// dropped, or `lifetime` passed.
    ///
    /// # Errors
    /// The server could not be reached or is no Nextcloud, the sign-in was cancelled or timed out.
    pub fn nextcloud_sign_in(
        &self,
        server: &str,
        page: impl FnOnce(String, bool),
        cancel: &std::sync::mpsc::Receiver<()>,
        interval: Duration,
        lifetime: Duration,
    ) -> Result<NextcloudSignedIn, String> {
        use std::sync::mpsc::RecvTimeoutError;
        match self.ask(&Request::NextcloudBegin {
            server: server.to_owned(),
        })? {
            Reply::NextcloudPage { url, shown } => page(url, shown),
            Reply::Failed { message } => return Err(message),
            _ => return Err(unexpected()),
        }
        let started = Instant::now();
        loop {
            match self.ask(&Request::NextcloudPoll)? {
                Reply::NextcloudWaiting => {}
                Reply::NextcloudSignedIn {
                    server,
                    login_name,
                    app_password,
                } => {
                    return Ok(NextcloudSignedIn {
                        server,
                        login_name,
                        app_password: Zeroizing::new(app_password),
                    });
                }
                Reply::Failed { message } => return Err(message),
                _ => return Err(unexpected()),
            }
            let remaining = lifetime.saturating_sub(started.elapsed());
            let ended = if remaining.is_zero() {
                Some("sign-in was not completed in time")
            } else {
                match cancel.recv_timeout(interval.min(remaining)) {
                    Err(RecvTimeoutError::Timeout) => None,
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => Some("sign-in was cancelled"),
                }
            };
            if let Some(ended) = ended {
                let _ = self.ask(&Request::NextcloudEnd);
                return Err(ended.to_owned());
            }
        }
    }
}

fn unexpected() -> String {
    "the session answered Settings out of turn: try again".to_owned()
}

impl Connection {
    /// Start signing in to the cloud storage `provider` (an account type id): the page the
    /// person approves it on, a code the session needs first, or the account itself when no
    /// page was needed.
    ///
    /// # Errors
    /// The provider is not configured in this build, or the connection failed.
    pub fn cloud_sign_in(
        &self,
        provider: &str,
        label: &str,
        apple_id: Option<&str>,
        password: Option<&str>,
        login_hint: Option<&str>,
    ) -> Result<CloudProgress, String> {
        let reply = self.ask(&Request::CloudSignIn {
            provider: provider.to_owned(),
            label: label.to_owned(),
            apple_id: apple_id.map(str::to_owned),
            password: password.map(str::to_owned),
            login_hint: login_hint.map(str::to_owned),
        })?;
        cloud_progress(reply)
    }

    /// Whether the cloud sign-in finished yet.
    ///
    /// # Errors
    /// The sign-in failed, or the connection did.
    pub fn cloud_poll(&self) -> Result<CloudProgress, String> {
        cloud_progress(self.ask(&Request::CloudPoll)?)
    }

    /// Answer the code the session asked for.
    ///
    /// # Errors
    /// The code was refused, the sign-in failed, or the connection did.
    pub fn cloud_code(&self, code: &str) -> Result<CloudProgress, String> {
        cloud_progress(self.ask(&Request::CloudCode {
            code: code.to_owned(),
        })?)
    }

    /// Stop following the cloud sign-in.
    pub fn cloud_end(&self) {
        let _ = self.ask(&Request::CloudEnd);
    }
}

fn cloud_progress(reply: Reply) -> Result<CloudProgress, String> {
    Ok(match reply {
        Reply::CloudPage { url, shown } => CloudProgress::Page { url, shown },
        Reply::CloudCode { message } => CloudProgress::Code { message },
        Reply::CloudWaiting => CloudProgress::Waiting,
        Reply::CloudSignedIn { account, secret } => {
            CloudProgress::SignedIn(CloudSignedIn { account, secret })
        }
        Reply::Failed { message } => return Err(message),
        _ => return Err(unexpected()),
    })
}

impl AccountTester for Connection {
    fn discover_calendars(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
    ) -> Result<Vec<tilo_dav::discovery::CalendarCollection>, String> {
        match self.ask(&Request::DiscoverCalendars {
            account: account.clone(),
            secret: secret.clone(),
        })? {
            Reply::Calendars { calendars } => Ok(calendars
                .into_iter()
                .map(|calendar| tilo_dav::discovery::CalendarCollection {
                    href: calendar.href,
                    name: calendar.name,
                    read_only: calendar.read_only,
                })
                .collect()),
            Reply::Failed { message } => Err(message),
            _ => Err(unexpected()),
        }
    }

    fn test(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
    ) -> Result<String, String> {
        match self.ask(&Request::Test {
            account: account.clone(),
            secret: secret.clone(),
        })? {
            Reply::Tested { said } => Ok(said),
            Reply::Failed { message } => Err(message),
            _ => Err(unexpected()),
        }
    }
}

fn write_line(stream: &mut UnixStream, message: &impl Serialize) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(message).map_err(std::io::Error::other)?;
    if line.len() > MAX_LINE_BYTES {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "a message is larger than the connection carries",
        ));
    }
    line.push(b'\n');
    stream.write_all(&line)?;
    stream.flush()
}

/// The next line, without its newline; `None` once the other side closed.
fn read_line(reader: &mut BufReader<UnixStream>) -> std::io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    let limit = u64::try_from(MAX_LINE_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let read = reader.by_ref().take(limit).read_until(b'\n', &mut line)?;
    if read == 0 {
        return Ok(None);
    }
    if line.last() != Some(&b'\n') {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "a line is longer than the connection carries",
        ));
    }
    line.pop();
    Ok(Some(line))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Says what it was asked, so a test reads what crossed the connection.
    struct Echo;

    impl AccountTester for Echo {
        fn discover_calendars(
            &self,
            account: &accounts::Account,
            _secret: &accounts::Secret,
        ) -> Result<Vec<tilo_dav::discovery::CalendarCollection>, String> {
            Ok(vec![tilo_dav::discovery::CalendarCollection {
                href: format!("/{}/personal/", account.id.0),
                name: "Personal".into(),
                read_only: false,
            }])
        }

        fn test(
            &self,
            account: &accounts::Account,
            secret: &accounts::Secret,
        ) -> Result<String, String> {
            if secret.password == "right" {
                Ok(format!("signed in to {}", account.id.0))
            } else {
                Err("IMAP server rejected the saved credential".into())
            }
        }
    }

    fn account() -> accounts::Account {
        accounts::Account::new("lab", "email")
    }

    #[test]
    fn settings_asks_and_the_session_answers_what_the_servers_said() {
        let (settings, session) = UnixStream::pair().expect("a pair");
        let serving = std::thread::spawn(move || serve(session, &Echo, &NoSignInPages));
        let connection = Connection::new(settings).expect("a connection");

        assert_eq!(
            connection.test(&account(), &accounts::Secret::password("right")),
            Ok("signed in to lab".to_owned())
        );
        assert_eq!(
            connection.test(&account(), &accounts::Secret::password("wrong")),
            Err("IMAP server rejected the saved credential".to_owned())
        );
        let calendars = connection
            .discover_calendars(&account(), &accounts::Secret::password("right"))
            .expect("the calendars");
        assert_eq!(calendars.len(), 1);
        assert_eq!(calendars[0].href, "/lab/personal/");

        // A poll before any sign-in began is refused, and the connection goes on.
        let (_stop, cancel) = std::sync::mpsc::channel::<()>();
        let refused = connection
            .nextcloud_sign_in(
                "ftp://cloud.example.test",
                |_, _| panic!("no page for a server that is not one"),
                &cancel,
                Duration::from_millis(10),
                Duration::from_secs(5),
            )
            .err()
            .expect("an address that is no Nextcloud server's");
        assert_eq!(refused, "use an HTTPS server URL");
        assert!(matches!(
            connection.ask(&Request::NextcloudPoll),
            Ok(Reply::Failed { .. })
        ));

        drop(connection);
        serving.join().expect("the session's end");
    }

    #[test]
    fn a_line_past_the_bound_ends_the_connection() {
        let (mut settings, session) = UnixStream::pair().expect("a pair");
        let serving = std::thread::spawn(move || serve(session, &Echo, &NoSignInPages));
        let long = vec![b'x'; MAX_LINE_BYTES + 2];
        let _ = settings.write_all(&long);
        serving.join().expect("the session's end");
        let mut rest = Vec::new();
        let _ = settings.read_to_end(&mut rest);
        assert!(
            rest.is_empty(),
            "nothing is answered to a line past the bound"
        );
    }
}
