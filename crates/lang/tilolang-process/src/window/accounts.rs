//! The person's website accounts (Slack, Discord), opened in a window of their own, each on a
//! profile of its own under Browser's data and signed in as that account.
//!
//! The account's cookies come from the workspace's vault, answered to Browser's window alone.
//! A profile takes them once: the engine sets them, is stopped and started again on the same
//! profile, and must read every one of them back before the profile is written down as holding
//! them ([`session_engine::NativeRestart`]). From then on a window opens the profile as it is,
//! and a site the person signed out of stays signed out. A profile is held by one window at a
//! time, and its engine stops when that window closes.

use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use browser_continuity::service_profiles::{GrantedRoot, Profile};
use browser_continuity::session_engine::{self, NativeRestart, Wake};
use browser_continuity::sessions::{Service, SessionCookies};
use tilo_app_window::page_runner::{AccountProfiles, Arrived};
use tilo_webview::chromium::ChromiumEngine;
use tilo_webview::{
    ServiceSessionReply, ServiceSessionRequest, SharedEngine, WebViewEngine as _, WebViewEvent,
};

/// How long the engine may take to answer that it took or read back an account's cookies.
const ANSWER: Duration = Duration::from_secs(45);

/// When an answer asked for now is due.
fn answer_by() -> Instant {
    let now = Instant::now();
    now.checked_add(ANSWER).unwrap_or(now)
}

/// Opens the person's website accounts on profiles of their own under Browser's data.
pub struct Accounts {
    /// The directory the profiles are kept under: Browser's own data, beside its profile.
    root: Option<PathBuf>,
}

impl Accounts {
    /// The accounts kept under Browser's own data, beside the profile its windows browse in.
    #[must_use]
    pub fn of_browser() -> Self {
        Self {
            root: tilo_webview::chromium::browsing_profile()
                .and_then(|profile| profile.parent().map(Path::to_owned)),
        }
    }
}

impl AccountProfiles for Accounts {
    fn open(&self, session: SessionCookies) -> Result<Arrived, String> {
        let service = session.service;
        let again = || format!("Sign in to {} again", service.label());
        let failed = || format!("{} could not be opened", service.label());
        if service == Service::Web || session.validate().is_err() || expired(&session) {
            return Err(again());
        }
        let root = self.root.as_deref().ok_or_else(failed)?;
        let root = GrantedRoot::admit_native(root).map_err(|_| failed())?;
        let mut profile = Profile::prepare(&root, &session).map_err(|error| {
            if error.kind() == std::io::ErrorKind::WouldBlock {
                format!("{} is open in another window", service.label())
            } else {
                tracing::warn!(%error, "an account's profile could not be prepared");
                failed()
            }
        })?;
        let path = profile.path().map_err(|_| failed())?.to_owned();
        let engine = match ChromiumEngine::with_service_session(&path, &session.session_id) {
            Ok(engine) => Engine(engine),
            Err(why) => {
                drop(profile.cancel_after_engine_exit());
                return Err(why);
            }
        };
        if !profile.needs_import() {
            return Ok(arrived(engine, profile, service));
        }
        let (bell, rung) = mpsc::channel();
        let wake: Wake = Arc::new(move || bell.send(()).unwrap_or_default());
        session_engine::Engine::attach_waker(&engine, wake.clone());
        let expected = (session.cookies.len(), session.origin_storage.len());
        let took = engine
            .0
            .import_service_session(ServiceSessionRequest {
                request_id: 1,
                verify_only: false,
                session: session.clone(),
            })
            .ok()
            .and_then(|()| answer(&engine, 1, &rung))
            .is_some_and(|reply| whole(&reply, &session, expected));
        if !took {
            close(engine, profile);
            return Err(again());
        }
        // The cookies hold only once a cold start of the engine reads every one of them back.
        let mut restart = NativeRestart::start(engine, profile, session.clone(), Some(wake));
        let deadline = answer_by();
        let (engine, mut profile) = loop {
            match restart.poll() {
                Some(Ok(restarted)) => break restarted,
                // The restart keeps the engine and the profile until the engine is confirmed
                // stopped.
                Some(Err(())) => return Err(again()),
                None => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() || rung.recv_timeout(left).is_err() {
                        restart.request_close();
                        return Err(again());
                    }
                }
            }
        };
        let read_back =
            answer(&engine, 2, &rung).is_some_and(|reply| whole(&reply, &session, expected));
        if !read_back || profile.complete().is_err() {
            close(engine, profile);
            return Err(again());
        }
        Ok(arrived(engine, profile, service))
    }
}

/// Whether any of the session's cookies ran out already.
fn expired(session: &SessionCookies) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(i64::MAX, |since| {
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
        });
    session.cookies.iter().any(|cookie| {
        cookie
            .expires_unix_seconds
            .is_some_and(|expiry| expiry <= now)
    })
}

/// Whether the engine's answer is about this session and counts every cookie and every site's
/// storage it was handed.
fn whole(
    reply: &ServiceSessionReply,
    session: &SessionCookies,
    (cookies, origins): (usize, usize),
) -> bool {
    reply.imported
        && reply.session_id == session.session_id
        && reply.service == session.service
        && reply.cookie_count == cookies
        && reply.origin_storage_count == origins
}

/// The engine's answer to the account's request `request_id`, waiting on `rung` until it comes
/// or [`ANSWER`] runs out. Nobody else reads the engine's events before the window has it.
fn answer(
    engine: &Engine,
    request_id: u32,
    rung: &mpsc::Receiver<()>,
) -> Option<ServiceSessionReply> {
    let deadline = answer_by();
    loop {
        let answered = engine
            .0
            .drain_events()
            .into_iter()
            .find_map(|event| match event {
                WebViewEvent::ServiceSessionResult(reply) if reply.request_id == request_id => {
                    Some(reply)
                }
                _ => None,
            });
        if answered.is_some() {
            return answered;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || rung.recv_timeout(left).is_err() {
            return None;
        }
    }
}

/// The account's engine for the window, at the service's own address, and what stops it and
/// lets go of its profile when the window closes.
fn arrived(engine: Engine, profile: Profile, service: Service) -> Arrived {
    Arrived {
        engine: Arc::clone(&engine.0) as SharedEngine,
        url: service.url().to_owned(),
        held: Box::new(Held(Some((engine, profile)))),
    }
}

/// Stop the account's engine and let go of its profile, off this thread: the profile is kept
/// as it is for the next window.
fn close(engine: Engine, profile: Profile) {
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    drop(session_engine::cleanup_after_native_exit(
        engine, profile, done, None,
    ));
}

/// The account's engine and profile, as long as the window that shows them is open.
struct Held(Option<(Engine, Profile)>);

impl Drop for Held {
    fn drop(&mut self) {
        if let Some((engine, profile)) = self.0.take() {
            close(engine, profile);
        }
    }
}

/// The account's engine, as the restart drives it.
#[derive(Clone)]
struct Engine(Arc<ChromiumEngine>);

impl session_engine::Engine for Engine {
    fn shutdown_service_session(&self) -> Result<(), String> {
        self.0.shutdown_service_session()
    }
    fn reopen(path: &Path, session_id: &str) -> Result<Self, String> {
        ChromiumEngine::with_service_session(path, session_id).map(Self)
    }
    fn attach_waker(&self, wake: Wake) {
        self.0.set_waker(Box::new(move || wake()));
    }
    fn verify(&self, session: SessionCookies) -> Result<(), String> {
        self.0.import_service_session(ServiceSessionRequest {
            request_id: 2,
            verify_only: true,
            session,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_continuity::sessions::{Cookie, SameSite};

    fn slack(cookie: &str) -> SessionCookies {
        SessionCookies {
            schema: 1,
            session_id: "slack-firefox-1".to_owned(),
            service: Service::Slack,
            identity_public_key: [7; 32],
            setup_binding: "workspace".to_owned(),
            source_snapshot_digest: [1; 32],
            cookies: vec![Cookie {
                host: "app.slack.com".to_owned(),
                domain: "app.slack.com".to_owned(),
                host_only: true,
                name: "d".to_owned(),
                value: cookie.to_owned(),
                path: "/".to_owned(),
                expires_unix_seconds: None,
                secure: true,
                http_only: true,
                same_site: SameSite::Lax,
            }],
            origin_storage: Vec::new(),
        }
    }

    /// Open `session` again once the window before let go of the profile.
    fn reopened(accounts: &Accounts, session: &SessionCookies) -> Result<Arrived, String> {
        let deadline = answer_by();
        loop {
            match accounts.open(session.clone()) {
                Err(why) if why.contains("another window") && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                opened => return opened,
            }
        }
    }

    /// A Slack account the person brought opens on a profile of its own: the engine takes its
    /// cookies, is stopped and started again, and reads every one back before the profile is
    /// written down as holding them. A second window waits for the first to close; a window after
    /// opens the profile as it is. An account whose cookies ran out asks the person to sign in
    /// again and opens nothing. It starts the Chromium engine host, which `TILO_CHROMIUM_HOST`
    /// names.
    #[test]
    #[ignore = "starts the Chromium engine host: run with --ignored where it is installed"]
    fn a_slack_account_opens_signed_in_on_a_profile_of_its_own() {
        let data = tempfile::tempdir().unwrap();
        let browser = data.path().join("tilo-browser");
        let accounts = Accounts {
            root: Some(browser.clone()),
        };
        let session = slack("made-up-slack-cookie");
        let receipt = browser
            .join("service-sessions")
            .join("07".repeat(32))
            .join("slack-firefox-1")
            .join("completed.json");

        let first = accounts.open(session.clone()).expect("the account opens");
        assert_eq!(first.url, "https://app.slack.com/");
        assert!(
            receipt.is_file(),
            "the profile is written down as holding the cookies once they were read back"
        );
        assert_eq!(
            accounts.open(session.clone()).err().as_deref(),
            Some("Slack is open in another window")
        );
        drop(first);
        let again = reopened(&accounts, &session).expect("the account opens again");
        assert_eq!(again.url, "https://app.slack.com/");
        drop(again);

        let mut ran_out = slack("made-up-slack-cookie");
        ran_out.session_id = "slack-firefox-2".to_owned();
        ran_out.cookies[0].expires_unix_seconds = Some(1);
        assert_eq!(
            accounts.open(ran_out).err().as_deref(),
            Some("Sign in to Slack again")
        );
    }
}
