//! Terminal's "This device" target, allowed by the person like any device of theirs.
//! The runtime (`ush`) refuses a login on the device's own shell until this session hands it the
//! person's answer. A window asks on its `shell-ui` connection
//! ([`FloatingSurfaceClientMessage::AllowHostShell`]): the answer is decided as every device answer
//! is ([`Consents::decide`]), kept where the person keeps those, handed to the supervisor (`ush`'s
//! `allow-host`), and the app is told on the same connection whether the target is open to it now
//! ([`FloatingSurfaceHostMessage::HostShellAllowed`]). When the person takes the answer back, here or
//! on another of their devices, the supervisor is told the target is closed again.

use std::io::{BufRead as _, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use collab_types::caps::DeviceCap;
use tilo_action_policy::Answer;
use tilo_action_types::Lifetime;
use tilo_peer_state::DeviceGrants;
use tilo_portal::PortalCaller;
use tilo_portal::consent::{AccessQuestion, Consents};
use ui::floating_surface::FloatingSurfaceHostMessage;

use crate::desktop_services::DesktopServices;

/// How long this session waits for the supervisor to take an answer before it takes it as not
/// taken.
const SUPERVISOR_ANSWER: Duration = Duration::from_secs(5);

/// The questions asked for the device's own shell that the person has not answered yet, and what
/// the supervisor was last handed.
#[derive(Default)]
pub(crate) struct HostShellAsks {
    asking: Vec<(u64, tokio::task::AbortHandle)>,
    /// The person's store's revision the answer was last looked at.
    followed: Option<u64>,
    /// What the supervisor was last handed.
    handed: Arc<Mutex<Handed>>,
}

/// What the supervisor was handed of the person's answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Handed {
    /// Nothing yet.
    #[default]
    Nothing,
    /// How long they allow the target; `None`: not at all.
    Answer(Option<Lifetime>),
}

impl HostShellAsks {
    /// Ask, for the app on `connection`, whether it may reach the device's own shell. The answer
    /// reaches the app on the same connection. An app whose launches are not handed `ush` has no
    /// use for it: it is told no without asking, and this returns false.
    pub(crate) fn ask(
        &mut self,
        services: Option<&DesktopServices>,
        app: &str,
        connection: u64,
        reply: Arc<Mutex<UnixStream>>,
    ) -> bool {
        let handed = crate::apps::declares_service(app, launcher::services::USH_SERVICE);
        let Some((services, consents)) = services
            .filter(|_| handed)
            .and_then(|services| Some((services, services.consents()?.clone())))
        else {
            // A session without the portal's answers has nobody to ask, and an app not handed
            // `ush` is asked nothing: the target stays closed and the app is told.
            answer(&reply, app, false);
            return handed;
        };
        // Asking again on the same connection replaces the question it is already waiting on rather
        // than stacking a second one.
        self.closed(connection);
        let app = app.to_owned();
        let last = Arc::clone(&self.handed);
        let asked = services.runtime().spawn(async move {
            let question = AccessQuestion {
                capability: DeviceCap::HostShell.name().to_owned(),
                reason: None,
                may_remember: true,
                text: None,
            };
            let handle = format!("shell-ui:{}:{connection}", DeviceCap::HostShell.name());
            let decided = consents
                .decide(
                    &PortalCaller::new(app.clone()),
                    &handle,
                    DeviceCap::HostShell,
                    question,
                )
                .await;
            // Refused or never answered, the target stays closed and the app is told so.
            let allowed = if decided.is_ok() {
                // An answer for once is kept nowhere: it stands for this one shell.
                let lifetime = allowed_for(consents.grants()).or(Some(Lifetime::Once));
                tokio::task::spawn_blocking(move || hand(&last, lifetime))
                    .await
                    .unwrap_or(false)
            } else {
                false
            };
            answer(&reply, &app, allowed);
        });
        self.asking.push((connection, asked.abort_handle()));
        true
    }

    /// The connection closed: the question it waits on goes with it, and the person is asked no
    /// more of it.
    pub(crate) fn closed(&mut self, connection: u64) {
        self.asking.retain(|(asking, question)| {
            let gone = *asking == connection || question.is_finished();
            if *asking == connection {
                question.abort();
            }
            !gone
        });
    }

    /// Hand the supervisor the person's answer whenever their store changed: an answer taken back
    /// on another of their devices closes the target here too.
    pub(crate) fn follow(&mut self, services: Option<&DesktopServices>) {
        let Some(services) = services else {
            return;
        };
        let Some(grants) = services.consents().map(Consents::grants) else {
            return;
        };
        let revision = grants.changes().map_or(0, |changes| *changes.borrow());
        if self.followed == Some(revision) {
            return;
        }
        self.followed = Some(revision);
        let lifetime = allowed_for(grants);
        if *self
            .handed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            == Handed::Answer(lifetime)
        {
            return;
        }
        let last = Arc::clone(&self.handed);
        services
            .runtime()
            .spawn_blocking(move || hand(&last, lifetime));
    }
}

/// How long the person's answer for the device's own shell stands on this device; `None` when it
/// does not allow it.
fn allowed_for(grants: &DeviceGrants) -> Option<Lifetime> {
    grants
        .given(tilo_app_id::apps::TERMINAL, DeviceCap::HostShell)
        .filter(|(answer, _)| *answer == Answer::Allow)
        .map(|(_, lifetime)| lifetime)
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

/// Hand `lifetime` to the supervisor (`None`: the target is closed), and note it in `last` when
/// it took it. Returns whether it did.
fn hand(last: &Mutex<Handed>, lifetime: Option<Lifetime>) -> bool {
    // The runtime's socket: a service of the login, outside every app sandbox, which is what lets
    // it take the answer this session gives it.
    let socket = launcher::services::socket_for(launcher::services::USH_SERVICE);
    let taken = ask_supervisor(&socket, lifetime, SUPERVISOR_ANSWER).unwrap_or_else(|error| {
        tracing::warn!(%error, "the supervisor did not take the answer for the device's own shell");
        false
    });
    if taken {
        *last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Handed::Answer(lifetime);
    }
    taken
}

/// One `allow-host` request on a fresh connection, waiting at most `patience` for the supervisor's
/// reply.
fn ask_supervisor(
    socket: &std::path::Path,
    lifetime: Option<Lifetime>,
    patience: Duration,
) -> std::io::Result<bool> {
    let stream = tilo_files::native::socket::connect(socket)?;
    stream.set_read_timeout(Some(patience))?;
    let mut line = serde_json::to_vec(&serde_json::json!({
        "op": "allow-host",
        "expiry": lifetime,
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
        assert!(asks.asking.is_empty(), "no question is left up");
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
    /// the person chose, and `null` once they took it back.
    #[test]
    fn the_allow_reaches_the_supervisor_as_the_runtime_reads_it() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("ush.sock");
        let asked = supervisor_at(&socket, r#"{"result":"allowed"}"#);
        assert!(
            ask_supervisor(&socket, Some(Lifetime::Once), Duration::from_secs(10)).unwrap(),
            "the supervisor records the allow"
        );
        // How long the allow lasts, as the runtime reads it.
        let asked = serde_json::from_str::<serde_json::Value>(&asked.recv().unwrap()).unwrap();
        assert_eq!(asked["expiry"], "once", "{asked}");
        assert_eq!(
            asked.as_object().map(serde_json::Map::len),
            Some(2),
            "{asked}"
        );
        let taken_back = dir.path().join("taken-back.sock");
        let asked = supervisor_at(&taken_back, r#"{"result":"allowed"}"#);
        assert!(ask_supervisor(&taken_back, None, Duration::from_secs(10)).unwrap());
        let asked = serde_json::from_str::<serde_json::Value>(&asked.recv().unwrap()).unwrap();
        assert!(asked["expiry"].is_null(), "{asked}");
    }

    #[test]
    fn an_allow_the_supervisor_did_not_record_is_not_taken_as_one() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("ush.sock");
        let _asked = supervisor_at(&socket, r#"{"result":"denied","reason":"not the shell"}"#);
        assert!(
            !ask_supervisor(
                &socket,
                Some(Lifetime::UntilRevoked),
                Duration::from_secs(10)
            )
            .unwrap(),
            "a refusal is not an allow"
        );
        assert!(
            ask_supervisor(
                &dir.path().join("absent.sock"),
                Some(Lifetime::UntilRevoked),
                Duration::from_secs(1)
            )
            .is_err(),
            "a supervisor that is not there records nothing"
        );
    }
}
