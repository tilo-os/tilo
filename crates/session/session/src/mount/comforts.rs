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

use settings_store::live::LiveSettings;
use settings_store::sign_in_comforts::SignInComforts;

/// The person's comforts as the login screens would take them, and whether
/// the person lets them follow.
fn persons() -> (SignInComforts, bool) {
    let access = tilo_accessibility_settings::AccessibilitySettings::load();
    let theme = ui::theme::Theme::configured_comforts();
    let defaults = SignInComforts::default();
    let comforts = SignInComforts {
        screen_reader: access.screen_reader,
        screen_reader_rate: access.screen_reader_rate,
        screen_reader_voice: access.screen_reader_voice,
        language: settings_store::appearance::LANGUAGE.get(),
        xkb_layout: None,
        xkb_variant: None,
        text_scale: theme.text_scale.unwrap_or(defaults.text_scale),
        high_contrast: theme.high_contrast.unwrap_or(defaults.high_contrast),
    }
    .with_keyboard_source(&settings_store::hardware::RichInputConfig::load().keyboard_layout)
    .clamped();
    (
        comforts,
        settings_store::sign_in_comforts::APPLY_TO_SIGN_IN.get(),
    )
}

/// The session's side of that: what the login screens were last told, and
/// the worker that tells them the next thing off the frame.
pub struct Mirror {
    /// Follows the person's settings, so a change is noticed once however
    /// many readers look. Between changes, looking costs one atomic load.
    live: LiveSettings,
    /// The generation `seen` was read at.
    generation: u64,
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
    pub fn mount(socket: PathBuf, wake: impl Fn() + Send + 'static) -> Self {
        Self {
            live: LiveSettings::start(wake),
            generation: 0,
            seen: None,
            worker: None,
            socket,
        }
    }

    /// Look at the person's comforts when their settings changed, and once
    /// at the start, which is where the session begins from.
    pub fn observe(&mut self) {
        let generation = self.live.generation();
        if self.seen.is_some() && generation == self.generation {
            return;
        }
        self.generation = generation;
        let (now, apply) = persons();
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
