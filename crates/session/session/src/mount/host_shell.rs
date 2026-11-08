//! Terminal's "This device" target, allowed once by the person.
//! The runtime (`ush`) refuses a login on the device's own shell until the person allowed it here.
//! A window asks on its `shell-ui` connection ([`FloatingSurfaceClientMessage::AllowHostShell`]):
//! this session puts its ordinary permission question in front of them, hands their answer to the
//! supervisor as the allow it records (`ush`'s `allow-host` ), and tells the app on the same
//! connection whether the target is open to it now
//! ([`FloatingSurfaceHostMessage::HostShellAllowed`]).
//! The question is answered between two of the session's passes, and the supervisor writes the
//! allow to disk before it answers: both run off the session's own pass, so a slow disk never holds
//! up the desk.

use std::io::{BufRead as _, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use collab_types::caps::Expiry;
use tilo_portal::consent::{
    ACCESS_QUESTION, ALLOW_ALWAYS, ALLOW_ONCE, ALLOW_SESSION, AccessQuestion,
};
use tilo_service::{Answer, Asked, Question};
use ui::floating_surface::FloatingSurfaceHostMessage;

use crate::desktop_services::DesktopServices;

/// The capability the question is worded for in the person's language (`portal-access-host-shell`).
pub const HOST_SHELL: &str = "host-shell";

/// The runtime's socket: a service of the login, outside every app sandbox,
/// which is what lets it take the allow this session gives it.
const USH_SERVICE: &str = "ush";

/// How long this session waits for the supervisor to record an allow before it takes the answer as
/// not recorded.
const SUPERVISOR_ANSWER: Duration = Duration::from_secs(5);

/// The questions asked for the device's own shell that the person has not answered yet.
#[derive(Default)]
pub(crate) struct HostShellAsks {
    waiting: Vec<Waiting>,
}

/// One question up for the person, and the connection waiting for its answer.
struct Waiting {
    connection: u64,
    asked: Asked,
    reply: Arc<Mutex<UnixStream>>,
    app: String,
}

impl HostShellAsks {
    /// Ask the person, for the app on `connection` , whether it may reach the device's own shell.
    /// The answer reaches the app on the same connection.
    pub(crate) fn ask(
        &mut self,
        services: Option<&DesktopServices>,
        app: &str,
        connection: u64,
        reply: Arc<Mutex<UnixStream>>,
    ) {
        let Some(services) = services else {
            // A session without the questions service has nobody to ask, so the target stays closed
            // and the app is told.
            answer(&reply, app, false);
            return;
        };
        // Asking again on the same connection replaces the question it is already waiting on rather
        // than stacking a second one.
        self.closed(connection);
        let details = serde_json::to_value(AccessQuestion {
            capability: HOST_SHELL.to_owned(),
            reason: None,
            may_remember: true,
            text: None,
        })
        .unwrap_or_default();
        let asked = services.questions().ask(Question {
            handle: format!("shell-ui:{HOST_SHELL}:{connection}"),
            kind: ACCESS_QUESTION.to_owned(),
            app_id: Some(app.to_owned()),
            label: crate::app_names::claimant(app),
            details,
        });
        self.waiting.push(Waiting {
            connection,
            asked,
            reply,
            app: app.to_owned(),
        });
    }

    /// The connection closed: the question it waits on goes with it, and the person is asked no
    /// more of it.
    pub(crate) fn closed(&mut self, connection: u64) {
        self.waiting
            .retain(|waiting| waiting.connection != connection);
    }

    /// Hand the person's answer to the supervisor and tell the app, for every question they
    /// answered since the last pass.
    pub(crate) fn settle(&mut self, services: Option<&DesktopServices>) {
        if self.waiting.is_empty() {
            return;
        }
        let waker = noop_waker();
        let mut still = Vec::with_capacity(self.waiting.len());
        for mut waiting in std::mem::take(&mut self.waiting) {
            let polled = {
                let mut context = std::task::Context::from_waker(&waker);
                std::pin::Pin::new(&mut waiting.asked).poll(&mut context)
            };
            let std::task::Poll::Ready(chosen) = polled else {
                still.push(waiting);
                continue;
            };
            // The question is answered: the app hears of it through the supervisor, which records
            // the allow before anything opens.
            let expiry = lifetime(chosen);
            let app = waiting.app;
            let reply = waiting.reply;
            match (expiry, services) {
                (Some(expiry), Some(services)) => {
                    services.runtime().spawn(async move {
                        let allowed = tokio::task::spawn_blocking(move || record(expiry))
                            .await
                            .unwrap_or(false);
                        answer(&reply, &app, allowed);
                    });
                }
                _ => answer(&reply, &app, false),
            }
        }
        self.waiting = still;
    }
}

/// The lifetime the person chose, in the runtime's vocabulary; `None` for a refusal.
fn lifetime(answer: Option<Answer>) -> Option<Expiry> {
    match answer?.choice {
        Some(ALLOW_ONCE) => Some(Expiry::AfterUse),
        Some(ALLOW_SESSION) => Some(Expiry::EndOfSession),
        Some(ALLOW_ALWAYS) => Some(Expiry::Never),
        _ => None,
    }
}

/// A waker that does nothing: the questions are looked at again on the next pass, so nothing waits
/// on being told they changed.
fn noop_waker() -> std::task::Waker {
    use std::task::{RawWaker, RawWakerVTable, Waker};
    const VTABLE: RawWakerVTable = RawWakerVTable::new(
        |_| RawWaker::new(std::ptr::null(), &VTABLE),
        |_| {},
        |_| {},
        |_| {},
    );
    // SAFETY: the vtable's functions do nothing with the pointer, which is
    // never dereferenced.
    unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) }
}

/// Tell the app on `reply` whether the device's own shell is open to it.
fn answer(reply: &Arc<Mutex<UnixStream>>, app: &str, allowed: bool) {
    let message = FloatingSurfaceHostMessage::HostShellAllowed { allowed };
    let written = reply
        .lock()
        .map_err(|_| "the connection's writer is gone".to_string())
        .and_then(|mut stream| {
            let mut frame = serde_json::to_vec(&message).map_err(|error| error.to_string())?;
            frame.push(b'\n');
            stream
                .write_all(&frame)
                .and_then(|()| stream.flush())
                .map_err(|error| error.to_string())
        });
    if let Err(error) = written {
        tracing::warn!(%error, %app, "the answer about the device's own shell was not written");
    }
}

/// Hand `expiry` to the supervisor, which records it where the device's own shell reads it. Returns
/// whether it did.
fn record(expiry: Expiry) -> bool {
    let socket = launcher::services::socket_for(USH_SERVICE);
    ask_supervisor(&socket, expiry, SUPERVISOR_ANSWER).unwrap_or_else(|error| {
        tracing::warn!(%error, "the allow for the device's own shell could not be recorded");
        false
    })
}

/// One `allow-host` request on a fresh connection, waiting at most `patience` for the supervisor's
/// reply.
fn ask_supervisor(
    socket: &std::path::Path,
    expiry: Expiry,
    patience: Duration,
) -> std::io::Result<bool> {
    let stream = tilo_files::native::socket::connect(socket)?;
    stream.set_read_timeout(Some(patience))?;
    let mut line = serde_json::to_vec(&serde_json::json!({
        "op": "allow-host",
        "expiry": expiry,
    }))
    .map_err(std::io::Error::other)?;
    line.push(b'\n');
    (&stream).write_all(&line)?;
    let mut reply = String::new();
    std::io::BufReader::new(&stream).read_line(&mut reply)?;
    Ok(matches!(
        serde_json::from_str(reply.trim()),
        Ok(ush_supervisor::ServeResponse::Allowed)
    ))
}

/// The supervisor's own words, as far as this session reads them
/// ([`ush::supervisor::ServeResponse`]): the allow was recorded, or it was not.
mod ush_supervisor {
    #[derive(Debug, serde::Deserialize)]
    #[serde(tag = "result", rename_all = "kebab-case")]
    pub(super) enum ServeResponse {
        Allowed,
        #[serde(other)]
        Another,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    /// What the person chose, as the questions service hands it back.
    fn chosen(choice: Option<usize>) -> Answer {
        Answer {
            choice,
            uris: Vec::new(),
            remember: false,
            values: Vec::new(),
        }
    }

    #[test]
    fn the_persons_choices_are_the_shared_lifetimes() {
        assert_eq!(
            lifetime(Some(chosen(Some(ALLOW_ONCE)))),
            Some(Expiry::AfterUse)
        );
        assert_eq!(
            lifetime(Some(chosen(Some(ALLOW_SESSION)))),
            Some(Expiry::EndOfSession)
        );
        assert_eq!(
            lifetime(Some(chosen(Some(ALLOW_ALWAYS)))),
            Some(Expiry::Never)
        );
        assert_eq!(lifetime(Some(chosen(None))), None, "a refusal");
        assert_eq!(
            lifetime(Some(chosen(Some(9)))),
            None,
            "no choice this session asks"
        );
        assert_eq!(lifetime(None), None, "the question went away");
    }

    /// A session without the questions service has nobody to ask, so the device's own shell stays
    /// closed and the app is told on its own connection.
    #[test]
    fn without_a_question_the_app_is_told_the_shell_stays_closed() {
        let (app, session) = UnixStream::pair().unwrap();
        app.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut asks = HostShellAsks::default();
        asks.ask(None, "so.tilo.terminal", 7, Arc::new(Mutex::new(session)));
        let mut line = String::new();
        BufReader::new(&app).read_line(&mut line).unwrap();
        assert_eq!(
            serde_json::from_str::<FloatingSurfaceHostMessage>(line.trim()).unwrap(),
            FloatingSurfaceHostMessage::HostShellAllowed { allowed: false }
        );
        assert!(asks.waiting.is_empty(), "no question is left up");
    }

    /// Hand `reply` to the first connection and return what was asked on it.
    fn supervisor_at(
        path: &std::path::Path,
        reply: &'static str,
    ) -> std::sync::mpsc::Receiver<String> {
        let listener = std::os::unix::net::UnixListener::bind(path).unwrap();
        let (asked, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            let _ = asked.send(line.trim().to_owned());
            (&stream)
                .write_all(format!("{reply}\n").as_bytes())
                .unwrap();
        });
        lines
    }

    /// The supervisor reads the request the runtime's own type writes: the allow, with the lifetime
    /// the person chose.
    #[test]
    fn the_allow_reaches_the_supervisor_as_the_runtime_reads_it() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("ush.sock");
        let asked = supervisor_at(&socket, r#"{"result":"allowed"}"#);
        assert!(
            ask_supervisor(&socket, Expiry::AfterUse, Duration::from_secs(10)).unwrap(),
            "the supervisor records the allow"
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&asked.recv().unwrap()).unwrap(),
            serde_json::json!({"op": "allow-host", "expiry": "AfterUse"})
        );
    }

    #[test]
    fn an_allow_the_supervisor_did_not_record_is_not_taken_as_one() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("ush.sock");
        let _asked = supervisor_at(&socket, r#"{"result":"denied","reason":"not the shell"}"#);
        assert!(
            !ask_supervisor(&socket, Expiry::Never, Duration::from_secs(10)).unwrap(),
            "a refusal is not an allow"
        );
        assert!(
            ask_supervisor(
                &dir.path().join("absent.sock"),
                Expiry::Never,
                Duration::from_secs(1)
            )
            .is_err(),
            "a supervisor that is not there records nothing"
        );
    }
}
