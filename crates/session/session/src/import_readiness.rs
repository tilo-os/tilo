//! The installed folder importer across this login: whether this session's first use is over,
//! answered on its own socket, and the review the person left to be opened once it is.
//! Add folders opens long after setup, on a saved import the person still has to finish. Before it
//! continues one it asks the session it is drawn in whether the identity ceremony is done, and only
//! continues on `ready` : nothing it carries may land under a half-made identity. The answer comes
//! from this session's own state, never from a flag or a file a caller could write, and the socket
//! it arrives on belongs to this session alone ([`tilo_paths::login::import_readiness_socket`],
//! named for the Wayland socket the person is looking at).
//! The review the importer saves asks, in `show_on_login` , to be opened again when the person next
//! logs in: the login opens it once their first use is over, instead of leaving them to remember
//! it. Both are hints and nothing more: the importer still opens its own private state under its
//! own lock and revalidates what it carries.

use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

/// What the importer reads while the identity ceremony is still under way.
const WAIT: &[u8] = b"tilo-import-wait--v1\n";
/// And once it is over.
const READY: &[u8] = b"tilo-import-ready-v1\n";

/// How many importers one pass answers before the desk draws.
const PER_PASS: usize = 8;

/// The desktop entry of the installed folder importer, which the login opens when the person left a
/// review asking to continue it.
pub(crate) const IMPORT_DESKTOP_ID: &str = "tilo-import-folders";

/// The most the saved review may be, as the importer writes it.
const REVIEW_BOUND: u64 = 256 * 1024;
/// The one byte past it that shows the file grew after it was measured.
const REVIEW_READ: u64 = REVIEW_BOUND + 1;

/// The installed folder importer's side of this login: the socket it asks on whether first use is
/// over, and the review it saved to be opened when it is.
pub(crate) struct Continuation {
    readiness: Readiness,
    /// The saved review asks to be opened once the person's first use is over.
    pending: bool,
    /// Whether this login already opened it: one login opens it once.
    opened: bool,
}

impl Continuation {
    /// Serve this session's readiness socket for `display`, the Wayland
    /// socket it just bound, and read the review the importer saved.
    ///
    /// # Errors
    /// There is no runtime directory to serve in, `display` is not a name a
    /// Wayland socket can have, or the socket cannot be bound.
    pub(crate) fn serve(display: &OsStr) -> io::Result<Self> {
        let readiness = Readiness::serve(display)?;
        Ok(Self {
            pending: saved_review_asks_to_open(readiness.account),
            readiness,
            opened: false,
        })
    }

    /// End the loop's wait when an importer asks, so it is answered on the
    /// pass that follows.
    ///
    /// # Errors
    /// The loop took no more sources.
    pub(crate) fn wake_on(
        &mut self,
        compositor: &mut tilo_compositor::Compositor,
    ) -> io::Result<()> {
        self.readiness.watch = Some(crate::sockets::watch(compositor, &self.readiness.listener)?);
        Ok(())
    }

    /// Answer every importer waiting since the last pass, and say whether the saved review opens
    /// now: once the person's first use is over, and once in this login. A client that does not
    /// read cannot stall a frame.
    pub(crate) fn follow(&mut self, first_use_over: bool) -> bool {
        self.readiness.follow(first_use_over);
        if !first_use_over || !self.pending || self.opened {
            return false;
        }
        self.opened = true;
        true
    }
}

/// The readiness socket of this session, answered on the session's own loop.
struct Readiness {
    listener: UnixListener,
    /// What ends the loop's wait while an importer asks.
    watch: Option<tilo_compositor::ReadableWatch>,
    path: PathBuf,
    /// The socket this instance bound: stopping a session never removes the socket of another one
    /// that bound the path after it.
    bound: (u64, u64),
    /// The account that bound it: the one whose saved review the login opens.
    account: u32,
}

impl Readiness {
    /// Serve this session's readiness socket for `display`, the Wayland
    /// socket it just bound.
    ///
    /// # Errors
    /// There is no runtime directory to serve in, `display` is not a name a
    /// Wayland socket can have, or the socket cannot be bound.
    fn serve(display: &OsStr) -> io::Result<Self> {
        let runtime = tilo_paths::xdg::runtime_dir().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "no runtime directory for import readiness",
            )
        })?;
        Self::serve_in(&runtime, display)
    }

    fn serve_in(runtime: &Path, display: &OsStr) -> io::Result<Self> {
        let directory = tilo_paths::login::session_dir(Some(runtime.as_os_str().to_owned()))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "no private directory for import readiness",
                )
            })?;
        let wayland = fs::symlink_metadata(runtime.join(display))?;
        let file = tilo_paths::login::import_readiness_socket_file(display, wayland.ino())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "the session's display cannot name a readiness socket",
                )
            })?;
        let path = directory.join(file);
        let listener = crate::agent_door::bind_private(&path)?;
        listener.set_nonblocking(true)?;
        let bound = fs::symlink_metadata(&path)?;
        Ok(Self {
            listener,
            watch: None,
            path,
            bound: (bound.dev(), bound.ino()),
            account: bound.uid(),
        })
    }

    /// Answer every importer waiting since the last pass: what a click of Add folders asks, from
    /// the session's state as this pass sees it. A client that does not read cannot stall a frame.
    fn follow(&self, first_use_over: bool) {
        let answer = if first_use_over { READY } else { WAIT };
        for _ in 0..PER_PASS {
            let Ok((mut client, _)) = self.listener.accept() else {
                return;
            };
            if client.set_nonblocking(true).is_err() {
                continue;
            }
            let _ = tilo_failure::ignored!(
                "a readiness client that left needs no answer",
                client.write_all(answer)
            );
            let _ = tilo_failure::ignored!(
                "a readiness client that left needs no answer",
                client.flush()
            );
        }
    }
}

impl Drop for Readiness {
    fn drop(&mut self) {
        let ours = fs::symlink_metadata(&self.path).is_ok_and(|current| {
            current.file_type().is_socket() && (current.dev(), current.ino()) == self.bound
        });
        if ours {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Whether the review the importer saved asks to be opened once first use is over: its own private
/// file of `account` 's, read as the importer wrote it.
#[allow(
    clippy::verbose_bit_mask,
    reason = "the POSIX 0o077 mask is the check that no one else may read or write the review"
)]
fn saved_review_asks_to_open(account: u32) -> bool {
    let Some(path) = tilo_paths::login::importer_continuation() else {
        return false;
    };
    let Ok(named) = fs::symlink_metadata(&path) else {
        return false;
    };
    // The importer writes it for this account alone: another account's file,
    // one anyone else may read, or a link to something else, is not a review
    // it saved.
    if !named.is_file()
        || named.uid() != account
        || named.mode() & 0o077 != 0
        || named.len() > REVIEW_BOUND
    {
        return false;
    }
    let Ok(mut file) = fs::File::open(&path) else {
        return false;
    };
    let Ok(opened) = file.metadata() else {
        return false;
    };
    if (opened.dev(), opened.ino()) != (named.dev(), named.ino()) {
        return false;
    }
    let mut bytes = Vec::new();
    if (&mut file)
        .take(REVIEW_READ)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() as u64 > REVIEW_BOUND
    {
        return false;
    }
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .inspect_err(|error| {
            tracing::warn!(%error, "the review record does not read back; the review is not shown");
        })
        .ok()
        .and_then(|review| {
            review
                .get("show_on_login")
                .and_then(serde_json::Value::as_bool)
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    /// A runtime directory with a display, and this session's private directory as the session
    /// makes it.
    fn login() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("a runtime directory");
        let root = temp.path().canonicalize().expect("a canonical path");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("private");
        fs::create_dir_all(root.join(tilo_paths::login::SESSION_DIR)).expect("the session's own");
        let _wayland = UnixListener::bind(root.join("wayland-1")).expect("a display");
        temp
    }

    fn socket_path(runtime: &Path) -> PathBuf {
        let wayland = fs::symlink_metadata(runtime.join("wayland-1")).expect("the display");
        tilo_paths::login::import_readiness_socket(
            Some(runtime.as_os_str().to_owned()),
            OsStr::new("wayland-1"),
            wayland.ino(),
        )
        .expect("the session's readiness path")
    }

    /// A click of Add folders: connected, and answered on the session's next pass.
    fn asking(path: &Path) -> UnixStream {
        let client = UnixStream::connect(path).expect("the readiness socket answers");
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("bounded");
        client
    }

    fn answer(client: &mut UnixStream) -> String {
        let mut answer = [0; READY.len()];
        client.read_exact(&mut answer).expect("one answer");
        String::from_utf8(answer.to_vec()).expect("text")
    }

    /// Add folders reads the session's own state: waiting while first use runs, ready after, and
    /// nothing else of this account can ask.
    #[test]
    fn the_readiness_socket_reports_wait_then_ready_and_goes_with_the_session() {
        let temp = login();
        let runtime = temp.path().canonicalize().expect("a canonical path");
        let path = socket_path(&runtime);
        let readiness = Readiness::serve_in(&runtime, OsStr::new("wayland-1")).expect("the socket");
        assert_eq!(
            fs::symlink_metadata(&path).expect("there").mode() & 0o777,
            0o600,
            "only this account may ask"
        );
        let mut waiting = asking(&path);
        readiness.follow(false);
        assert_eq!(answer(&mut waiting), String::from_utf8_lossy(WAIT));
        let mut ready = asking(&path);
        readiness.follow(true);
        assert_eq!(answer(&mut ready), String::from_utf8_lossy(READY));
        drop(readiness);
        assert!(
            fs::symlink_metadata(&path).is_err(),
            "the socket goes with the session"
        );
    }

    /// One session answers for one display: a second one on the socket the first serves is refused
    /// rather than taking over its answers.
    #[test]
    fn a_served_readiness_socket_is_not_taken_over() {
        let temp = login();
        let runtime = temp.path().canonicalize().expect("a canonical path");
        let first = Readiness::serve_in(&runtime, OsStr::new("wayland-1")).expect("the first");
        assert!(
            Readiness::serve_in(&runtime, OsStr::new("wayland-1")).is_err(),
            "a live session keeps its own socket"
        );
        assert!(
            Readiness::serve_in(&runtime, OsStr::new("../escape")).is_err(),
            "a display that could name a socket outside the session dir is refused"
        );
        drop(first);
    }

    /// The review the person left opens once their first use is over, and one login opens it once
    /// however many passes it draws.
    #[test]
    fn a_saved_review_opens_once_first_use_is_over() {
        let temp = login();
        let runtime = temp.path().canonicalize().expect("a canonical path");
        let readiness = Readiness::serve_in(&runtime, OsStr::new("wayland-1")).expect("the socket");
        let mut continuation = Continuation {
            readiness,
            pending: true,
            opened: false,
        };
        assert!(
            !continuation.follow(false),
            "nothing opens under an identity ceremony still under way"
        );
        assert!(
            continuation.follow(true),
            "the saved review opens once first use is over"
        );
        assert!(
            !continuation.follow(true),
            "and this login opens it no second time"
        );
    }

    /// The importer writes its review for this account alone: a review any other account could have
    /// written opens nothing.
    #[test]
    fn only_the_importers_own_private_review_asks_to_open() {
        let home = tilo_test_env::TestHome::new().expect("this login's home");
        let review = tilo_paths::login::importer_continuation().expect("the importer's review");
        let account = std::fs::symlink_metadata(home.data())
            .expect("this account's directory")
            .uid();
        assert!(
            !saved_review_asks_to_open(account),
            "no review, nothing to open"
        );

        std::fs::create_dir_all(review.parent().expect("the importer's directory"))
            .expect("the importer's directory");
        let write = |asked: &str, mode: u32| {
            std::fs::write(&review, asked).expect("a review");
            std::fs::set_permissions(&review, std::fs::Permissions::from_mode(mode))
                .expect("its permissions");
        };
        write(r#"{"show_on_login":true}"#, 0o600);
        assert!(
            saved_review_asks_to_open(account),
            "the person asked to continue it"
        );
        write(r#"{"show_on_login":false}"#, 0o600);
        assert!(
            !saved_review_asks_to_open(account),
            "a review finished or put aside opens nothing"
        );
        write(r#"{"show_on_login":true}"#, 0o644);
        assert!(
            !saved_review_asks_to_open(account),
            "a review anyone else may write is not the importer's"
        );
        write(r#"{"show_on_login":true}"#, 0o600);
        assert!(
            !saved_review_asks_to_open(account.saturating_add(1)),
            "a file another account left opens nothing"
        );
    }
}
