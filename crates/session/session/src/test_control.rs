//! The development test-control socket: `tilo-control` drives a session it was started for
//! (`tilo-session --test-control PATH`, honoured by development builds only) over the
//! protocol it speaks to the shell's (`shell::shell::test_control`): one JSON request per line,
//! tagged by `op`, and one answer per line, `{"ok": {"frame": N}}` or
//! `{"err": {"code": .., "message": ..}}`.
//!
//! The session answers `snapshot` (`tilo-control snapshot PATH`): a PNG of every display, each at
//! its place in the layout, as the portal's screenshot of the screen takes it
//! ([`crate::capture::picture`]); and `provision_test_identity`
//! (`tilo-control raw '{"op":"provision_test_identity"}'`), which mints the throwaway identity a
//! development session opens its desk with instead of first-use setup, as the shell's does for a
//! dev image; and `click_at` (`tilo-control click-at X Y`), a click of the primary button there,
//! which a development run presses the screen's own controls with (the shell's preset switch).
//! Every other request is answered `unsupported`.
//!
//! Each connection is read on a thread of its own; what it asks is answered on the loop's pass,
//! the only place that may touch the compositor, and the loop is woken for it.

use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use serde_json::{Value as Json, json};
use tilo_compositor::Compositor;
use tilo_service::Wake;

/// The variable `tilo-control` reads the test-control socket's path from, which the session
/// prints with the path it opened.
pub const ENV: &str = "TILO_TEST_CONTROL";

/// Whether this build opens the test-control socket a command line names: development builds
/// only. A release build leaves the flag without effect, so nothing it ships drives a session
/// as the person through a socket.
pub const OPENED_BY_THIS_BUILD: bool = cfg!(debug_assertions);

/// Where the socket `--test-control` names is opened, given whether this build opens one
/// (`opens`, [`OPENED_BY_THIS_BUILD`]) and the path the flag named: `None` in a build that
/// opens none, whatever was named.
#[must_use]
pub fn path(opens: bool, named: Option<&str>) -> Option<PathBuf> {
    named.filter(|_| opens).map(PathBuf::from)
}

/// The socket at `path`, when one is named, opened; `None`, saying why, when
/// it does not open.
pub(crate) fn open_named(path: Option<&Path>, wake: Wake) -> Option<TestControl> {
    let path = path?;
    match TestControl::open(path, wake) {
        Ok(control) => Some(control),
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "the test-control socket does not open");
            None
        }
    }
}

/// Answer what `control`, when there is one, was asked since the last pass.
pub(crate) fn serve(control: Option<&TestControl>, compositor: &mut Compositor) {
    if let Some(control) = control {
        control.serve(compositor);
    }
}

/// One request a connection sent, and where its answer goes.
struct Asked {
    request: String,
    answer: Sender<String>,
}

/// The socket, served on the loop's pass.
pub(crate) struct TestControl {
    listener: UnixListener,
    path: PathBuf,
    asks: Sender<Asked>,
    asked: Receiver<Asked>,
    wake: Wake,
}

impl TestControl {
    /// Open the socket at `path`, for this account alone; `wake` wakes the loop when a
    /// connection asks something.
    ///
    /// # Errors
    /// The path is not a socket, another session answers on it, or it cannot be bound.
    pub(crate) fn open(path: &Path, wake: Wake) -> io::Result<Self> {
        let listener = crate::agent_door::bind_private(path)?;
        listener.set_nonblocking(true)?;
        let (asks, asked) = channel();
        Ok(Self {
            listener,
            path: path.to_path_buf(),
            asks,
            asked,
            wake,
        })
    }

    /// Take the connections waiting and answer what was asked since the last pass.
    pub(crate) fn serve(&self, compositor: &mut Compositor) {
        self.accept();
        while let Ok(Asked { request, answer }) = self.asked.try_recv() {
            let _ = answer.send(answered(&request, compositor).to_string());
        }
    }

    /// Accept every waiting connection, each read on its own thread.
    fn accept(&self) {
        loop {
            let stream = match self.listener.accept() {
                Ok((stream, _)) => stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
                Err(error) => {
                    tracing::warn!(%error, "test control: accepting a connection failed");
                    return;
                }
            };
            let asks = self.asks.clone();
            let wake = std::sync::Arc::clone(&self.wake);
            let spawned = std::thread::Builder::new()
                .name("tilo-test-control".into())
                .spawn(move || serve_connection(stream, &asks, &wake));
            if let Err(error) = spawned {
                tracing::warn!(%error, "test control: a connection could not be served");
            }
        }
    }
}

impl Drop for TestControl {
    fn drop(&mut self) {
        // Nothing answers there once the session is gone.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Read `stream`'s requests one line at a time, hand each to the loop and write its answer,
/// until the client closes it or the session ends.
fn serve_connection(stream: UnixStream, asks: &Sender<Asked>, wake: &Wake) {
    if let Err(error) = stream.set_nonblocking(false) {
        tracing::debug!(%error, "test control: a connection cannot block");
        return;
    }
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    for line in BufReader::new(stream).lines() {
        let Ok(request) = line else {
            return;
        };
        if request.trim().is_empty() {
            continue;
        }
        let (answer, answered) = channel();
        if asks.send(Asked { request, answer }).is_err() {
            return;
        }
        wake();
        // The loop answers every request it takes; a session that ends drops it unanswered.
        let Ok(reply) = answered.recv() else {
            return;
        };
        if writer.write_all(format!("{reply}\n").as_bytes()).is_err() {
            return;
        }
    }
}

/// The answer to `request`, run against `compositor`.
fn answered(request: &str, compositor: &mut Compositor) -> Json {
    let request: Json = match serde_json::from_str(request) {
        Ok(request) => request,
        Err(error) => return refusal("parse_error", &error.to_string()),
    };
    match request.get("op").and_then(Json::as_str) {
        Some("snapshot") => {
            let path = request
                .get("path")
                .and_then(Json::as_str)
                .unwrap_or_default();
            match snapshot(compositor, path) {
                Ok(()) => json!({"ok": {"frame": 0}}),
                Err((code, message)) => refusal(code, &message),
            }
        }
        Some("provision_test_identity") => provision_test_identity(),
        Some("click_at") => {
            let at = |name: &str| request.get(name).and_then(Json::as_f64);
            match (at("x"), at("y")) {
                (Some(x), Some(y)) => match click_at(compositor, x, y) {
                    Ok(()) => json!({"ok": {}}),
                    Err(error) => refusal("input_failed", &error),
                },
                _ => refusal("parse_error", "`click_at` takes `x` and `y`"),
            }
        }
        Some(op) => refusal(
            "unsupported",
            &format!(
                "this session's test control answers `snapshot`, `click_at` and \
                 `provision_test_identity`, not `{op}`"
            ),
        ),
        None => refusal("parse_error", "a request names its `op`"),
    }
}

/// A press and release of the primary button at `(x, y)`, logical pixels of the layout, as the
/// person clicks there: what a development run drives the screen's own controls with.
fn click_at(compositor: &mut Compositor, x: f64, y: f64) -> Result<(), String> {
    use tilo_compositor::InputEvent;
    const BTN_LEFT: u32 = 0x110;
    compositor
        .input(InputEvent::PointerMotion { x, y })
        .map_err(|error| error.to_string())?;
    for pressed in [true, false] {
        compositor
            .input(InputEvent::PointerButton {
                button: BTN_LEFT,
                pressed,
            })
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Write a PNG of every display, each at its place in the layout, to `path`.
fn snapshot(compositor: &mut Compositor, path: &str) -> Result<(), (&'static str, String)> {
    if path.is_empty() {
        return Err(("invalid_path", "snapshot path must not be empty".to_owned()));
    }
    let picture = crate::capture::picture(compositor).map_err(|why| ("snapshot_failed", why))?;
    picture
        .save_with_format(path, image::ImageFormat::Png)
        .map_err(|error| ("snapshot_failed", format!("{path}: {error}")))
}

/// Mint a throwaway identity for this session's user, the one a session started after it opens
/// its desk with: what `provision_test_identity` answers on the shell's socket, in the same
/// shape. A real identity is never replaced; a release build mints none.
fn provision_test_identity() -> Json {
    use tilo_identity_host::provisioning::{ProvisionOutcome, THROWAWAY_TEST_IDENTITY_NAME};
    let store = match tilo_onboarding_host::identity_provisioning::DiskIdentityStore::from_env() {
        Ok(store) => store,
        Err(error) => return refusal("provision_failed", &format!("no identity store: {error}")),
    };
    let Some(identity_dir) = store.path().parent() else {
        return refusal("provision_failed", "the identity store has no directory");
    };
    match tilo_onboarding_host::firstboot::ensure_throwaway_test_identity(
        identity_dir,
        &tilo_onboarding_host::firstboot::collaboration_identity_paths(),
    ) {
        Ok(outcome) => json!({"answer": {
            "created": matches!(outcome, ProvisionOutcome::Created { .. }),
            "display_name": THROWAWAY_TEST_IDENTITY_NAME,
            "throwaway": true,
        }}),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            refusal("identity_exists", &error.to_string())
        }
        Err(error) if error.kind() == io::ErrorKind::Unsupported => {
            refusal("dev_image_only", &error.to_string())
        }
        Err(error) => refusal("provision_failed", &error.to_string()),
    }
}

fn refusal(code: &str, message: &str) -> Json {
    json!({"err": {"code": code, "message": message}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_development_build_opens_the_socket_it_is_named() {
        assert_eq!(
            path(true, Some("/run/user/1000/control.sock")),
            Some(PathBuf::from("/run/user/1000/control.sock"))
        );
        assert_eq!(path(false, Some("/run/user/1000/control.sock")), None);
        assert_eq!(path(true, None), None);
        assert_eq!(OPENED_BY_THIS_BUILD, cfg!(debug_assertions));
    }
}
