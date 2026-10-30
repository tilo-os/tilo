//! The login screens follow the comforts a person turns on in their session:
//! the screen reader, text size, contrast, language and keyboard layout.
//! `tilo-authd` keeps the device's copy and mirrors it into the volume header
//! for the boot.
//!
//! Only a change is carried over, and only the fields that changed: signing
//! in leaves the login screens as they are, and a person who makes their text
//! larger does not turn off the screen reader someone turned on at the login
//! screen. Their `apply_to_sign_in` switch keeps the two apart.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use settings_store::sign_in_comforts::SignInComforts;
use settings_store::watch::SettingsWatch;

use crate::SessionConfig;

/// The session's side of that: what the login screens were last told, and
/// the worker that tells them the next thing off the frame.
pub struct Mirror {
    /// The comforts the session's settings loader read, newest last.
    updates: mpsc::Receiver<(SignInComforts, bool)>,
    /// Keeps the loader's notifications coming; dropping it stops them, as
    /// it does for every other reader of the loader.
    _following: Option<SettingsWatch>,
    /// What the login screens were last told, or what they already hold when
    /// the session started.
    seen: Option<SignInComforts>,
    /// Sends each change to `tilo-authd` in order, off the frame.
    worker: Option<mpsc::Sender<serde_json::Map<String, serde_json::Value>>>,
    /// The daemon's socket: the device's, or a stand-in's in a test.
    socket: PathBuf,
}

impl Mirror {
    /// Follow this session's settings and tell the daemon at `socket` about
    /// the comforts that change; `wake` brings the session's loop back to
    /// look.
    pub fn mount(socket: PathBuf, wake: impl Fn() + Send + Sync + 'static) -> Self {
        let (sender, updates) = mpsc::channel();
        // The loader's notifications come from whichever thread writes the
        // settings; the sender is shared through a lock so it can.
        let sender = std::sync::Mutex::new(sender);
        let following = SessionConfig::follow(move |config| {
            let comfort = (config.comforts, config.apply_comforts_to_sign_in);
            sender
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .send(comfort);
            wake();
        })
        .inspect_err(
            |error| tracing::info!(%error, "the sign-in comforts do not follow the settings"),
        )
        .ok();
        Self {
            updates,
            _following: following,
            seen: None,
            worker: None,
            socket,
        }
    }

    /// Look at the person's comforts when their settings changed, and once
    /// at the start, which is where the session begins from.
    pub fn observe(&mut self) {
        let mut latest = None;
        while let Ok(next) = self.updates.try_recv() {
            latest = Some(next);
        }
        let (now, apply) = match latest {
            Some(next) => next,
            // The first look is where the session begins from: the settings
            // the session started with, which the login screens already hold.
            None if self.seen.is_none() => {
                let config = SessionConfig::load();
                (config.comforts, config.apply_comforts_to_sign_in)
            }
            None => return,
        };
        if let Some(changes) = change(&mut self.seen, now, apply) {
            self.send(changes);
        }
    }

    /// Hand `changes` to the worker, starting it the first time.
    fn send(&mut self, changes: serde_json::Map<String, serde_json::Value>) {
        let socket = self.socket.clone();
        let worker = self.worker.get_or_insert_with(|| {
            let (sender, changes) = mpsc::channel();
            std::thread::Builder::new()
                .name("sign-in-comforts".into())
                .spawn(move || {
                    for changes in changes {
                        mirror(&socket, changes);
                    }
                })
                .map_err(|error| tracing::warn!(%error, "sign-in comforts worker did not start"))
                .ok();
            sender
        });
        if worker.send(changes).is_err() {
            self.worker = None;
        }
    }
}

/// The fields to change at the login screens, if `now` changed what they
/// take and the person lets them follow. The first look is where the session
/// starts from, not a change.
fn change(
    seen: &mut Option<SignInComforts>,
    now: SignInComforts,
    apply: bool,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let before = seen.replace(now)?;
    let changes = SignInComforts::changes(&before, seen.as_ref()?);
    (apply && !changes.is_empty()).then_some(changes)
}

/// Tell `socket`'s daemon the fields `changes` names, and keep the rest.
fn mirror(socket: &Path, changes: serde_json::Map<String, serde_json::Value>) {
    use tilo_auth::protocol::Request;
    let reply = tilo_auth::client::Client::connect(socket)
        .and_then(|mut client| client.call(&Request::SetSignInComforts { comforts: changes }));
    match reply {
        Ok(reply) if reply.ok => {}
        Ok(reply) => tracing::warn!(?reply.error, "the login screens kept their comforts"),
        Err(error) => tracing::debug!(%error, "no sign-in daemon to take the comforts"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_change_the_person_lets_through_reaches_the_login_screens() {
        let mut seen = None;
        let start = SignInComforts::default().with_keyboard_source("de");
        assert_eq!(change(&mut seen, start.clone(), true), None);
        assert_eq!(change(&mut seen, start.clone(), true), None);
        let reader = SignInComforts {
            screen_reader: true,
            ..start.clone()
        };
        assert_eq!(
            change(&mut seen, reader.clone(), true),
            serde_json::json!({"screen_reader": true})
                .as_object()
                .cloned()
        );
        let larger = SignInComforts {
            text_scale: 1.5,
            ..reader
        };
        assert_eq!(change(&mut seen, larger, false), None);
        assert_eq!(
            change(&mut seen, start, true),
            serde_json::json!({"screen_reader": false, "text_scale": 1.0})
                .as_object()
                .cloned()
        );
    }

    #[test]
    fn a_change_reaches_the_daemon_that_keeps_the_login_screens_comforts() {
        use std::io::{BufRead as _, Write as _};
        let directory = tempfile::tempdir().expect("temporary directory");
        let socket = directory.path().join("socket");
        let server = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
        let (asked, asked_changes) = mpsc::channel();
        std::thread::spawn(move || {
            let (stream, _) = server.accept().expect("a client");
            let mut line = String::new();
            std::io::BufReader::new(&stream)
                .read_line(&mut line)
                .expect("the request");
            asked.send(line).expect("the test listens");
            (&stream).write_all(b"{\"ok\":true}\n").expect("the reply");
        });

        let mut mirror = Mirror::mount(socket, || {});
        mirror.send(
            serde_json::json!({"text_scale": 1.5})
                .as_object()
                .cloned()
                .expect("an object"),
        );
        let line = asked_changes.recv_timeout(std::time::Duration::from_secs(10));
        let asked = line.expect("the daemon was told");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&asked).expect("json"),
            serde_json::json!({
                "op": "set-sign-in-comforts",
                "comforts": {"text_scale": 1.5},
            }),
            "the daemon is told the fields that changed, and nothing else"
        );
    }
}
