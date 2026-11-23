//! What is booted, staged and rolled back to on this machine, for its admin without root.
//!
//! Which deployments are on the disk is known only to root: `bootc status` refuses everyone
//! else, and the ESP is mounted root-only because it holds the boot loader's random seed.
//! `tilo-deployment-status.socket` (group `wheel`) runs `tilo-deployment serve-status` per
//! connection, which takes one word, `text` or `json`, and answers with the listing's exit
//! status on the first line and the listing after it. `json` is bootc's own status.
//!
//! This is the one client of that socket: `tilo status` prints what it answers, and the
//! session's system service reads the deployments from it ([`read`]).

use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub use tilo_system_resources::update::Deployment;

/// Where the status service listens.
pub const SOCKET: &str = "/run/tilo-deployment/status.sock";

/// Above the service's own `TimeoutStartSec=`, so its verdict arrives before this gives up.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(75);

/// The most a listing may be: bootc's status of a few deployments is a few kilobytes.
const MOST: u64 = 4 << 20;

/// Where the service listens, or `None` on a system that has no such socket (an image older
/// than it). `TILO_DEPLOYMENT_STATUS_SOCKET` names another, for a test.
#[must_use]
pub fn socket() -> Option<PathBuf> {
    let path = std::env::var_os("TILO_DEPLOYMENT_STATUS_SOCKET")
        .map_or_else(|| PathBuf::from(SOCKET), Into::into);
    path.exists().then_some(path)
}

/// The listing to ask for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Listing {
    /// The text `tilo status` prints.
    Text,
    /// bootc's status, as JSON.
    Json,
}

/// The listing's own exit status and text.
#[derive(Debug, PartialEq, Eq)]
pub struct Answer {
    pub code: u8,
    pub text: String,
}

/// Why the status could not be asked.
#[derive(Debug)]
pub enum AskError {
    /// The account is not one of the machine's admins: the socket answers only them.
    NotAnAdmin,
    /// The socket could not be reached or read.
    Io {
        doing: &'static str,
        error: std::io::Error,
    },
    /// The service ended without an answer.
    NoAnswer,
    /// The listing failed with its own exit status and words.
    Failed(Answer),
    /// The listing does not read as bootc's status.
    Unreadable(String),
}

impl fmt::Display for AskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAnAdmin => f.write_str("what is on this machine is shown to its admins"),
            Self::Io { doing, error } => write!(f, "{doing}: {error}"),
            Self::NoAnswer => f.write_str(
                "the status service gave no answer; \
                 `journalctl -u 'tilo-deployment-status@*'` says why",
            ),
            Self::Failed(answer) => write!(
                f,
                "the status listing ended with {}: {}",
                answer.code,
                answer.text.trim()
            ),
            Self::Unreadable(error) => write!(f, "the status does not read: {error}"),
        }
    }
}

impl std::error::Error for AskError {}

/// Ask the service at `socket` for `listing`.
///
/// # Errors
/// The account may not ask, the socket is not there or does not answer.
pub fn ask(socket: &Path, listing: Listing) -> Result<Answer, AskError> {
    let io = |doing| move |error| AskError::Io { doing, error };
    let mut stream = match tilo_files::native::socket::connect(socket) {
        Ok(stream) => stream,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            return Err(AskError::NotAnAdmin);
        }
        Err(error) => return Err(io("connecting to the status service")(error)),
    };
    stream
        .set_read_timeout(Some(ANSWER_TIMEOUT))
        .map_err(io("waiting for the status"))?;
    stream
        .write_all(match listing {
            Listing::Text => b"text\n",
            Listing::Json => b"json\n",
        })
        .map_err(io("asking for the status"))?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(io("asking for the status"))?;
    let mut raw = String::new();
    stream
        .take(MOST)
        .read_to_string(&mut raw)
        .map_err(io("reading the status"))?;
    parse(&raw)
}

fn parse(raw: &str) -> Result<Answer, AskError> {
    let (first, text) = raw.split_once('\n').unwrap_or((raw, ""));
    let code = first.trim().parse::<u8>().map_err(|_| AskError::NoAnswer)?;
    Ok(Answer {
        code,
        text: text.to_owned(),
    })
}

/// The deployments on this machine, as the service at `socket` lists them.
///
/// # Errors
/// As [`ask`]; a listing that failed or does not read as bootc's status.
pub fn read(socket: &Path) -> Result<Deployments, AskError> {
    let answer = ask(socket, Listing::Json)?;
    if answer.code != 0 {
        return Err(AskError::Failed(answer));
    }
    Deployments::parse(&answer.text).map_err(AskError::Unreadable)
}

/// What is booted, staged for the next start and kept to go back to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Deployments {
    pub booted: Option<Deployment>,
    pub staged: Option<Deployment>,
    pub rollback: Option<Deployment>,
}

impl Deployments {
    /// The deployments `bootc status --format json` lists.
    ///
    /// # Errors
    /// `json` is not bootc's status.
    pub fn parse(json: &str) -> Result<Self, String> {
        let root: serde_json::Value =
            serde_json::from_str(json).map_err(|error| error.to_string())?;
        let status = root
            .get("status")
            .ok_or_else(|| "it names no status".to_owned())?;
        let deployment = |slot: &str| {
            let entry = status.get(slot).filter(|entry| !entry.is_null())?;
            let image = entry.get("image");
            let text = |path: &[&str]| {
                path.iter()
                    .try_fold(image?, |value, key| value.get(key))?
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
            };
            Some(Deployment {
                image: text(&["image", "image"]),
                digest: text(&["imageDigest"]),
                version: text(&["version"]),
                built: text(&["timestamp"]),
            })
        };
        Ok(Self {
            booted: deployment("booted"),
            staged: deployment("staged"),
            rollback: deployment("rollback"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serve_once(
        answer: &'static str,
    ) -> (
        tempfile::TempDir,
        std::path::PathBuf,
        std::thread::JoinHandle<String>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("status.sock");
        let listener = tilo_files::native::socket::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            stream.read_to_string(&mut request).unwrap();
            stream.write_all(answer.as_bytes()).unwrap();
            request
        });
        (dir, path, server)
    }

    /// bootc's status of a machine that runs one image, has the next staged and keeps the one
    /// before, trimmed to what is read of it.
    const STATUS: &str = r#"0
{"apiVersion":"org.containers.bootc/v1","kind":"BootcHost","status":{
 "booted":{"image":{"image":{"image":"registry.tilo.so/tilo/tilo-vm:x86_64-ci","transport":"registry"},
   "imageDigest":"sha256:7ea8","timestamp":"2026-11-21T06:25:11.272075619Z","version":"44.20261121.0"}},
 "staged":{"image":{"image":{"image":"registry.tilo.so/tilo/tilo-vm:x86_64-ci","transport":"registry"},
   "imageDigest":"sha256:fbd4","timestamp":"2026-11-21T09:33:50Z","version":null}},
 "rollback":null}}
"#;

    tilo_test_env::scenario! {
        fn the_deployment_status_is_asked_over_its_socket;

        phase asks_for_one_word_and_returns_the_listing_with_its_status;
        {
            let (_dir, path, server) = serve_once("0\nDeployments:\n  booted    image\n");
            let answer = ask(&path, Listing::Text).unwrap();
            assert_eq!(server.join().unwrap(), "text\n");
            assert_eq!(
                answer,
                Answer {
                    code: 0,
                    text: "Deployments:\n  booted    image\n".to_owned()
                }
            );
        }

        phase json_is_its_own_request_and_reads_as_the_deployments;
        {
            let (_dir, path, server) = serve_once(STATUS);
            let deployments = read(&path).unwrap();
            assert_eq!(server.join().unwrap(), "json\n");
            let booted = deployments.booted.unwrap();
            assert_eq!(
                booted.image.as_deref(),
                Some("registry.tilo.so/tilo/tilo-vm:x86_64-ci")
            );
            assert_eq!(booted.digest.as_deref(), Some("sha256:7ea8"));
            assert_eq!(booted.name(), Some("44.20261121.0"), "a version names it first");
            assert_eq!(booted.built_on(), Some("2026-11-21"));
            let staged = deployments.staged.unwrap();
            assert_eq!(staged.version, None, "an image without a version has none");
            assert_eq!(
                staged.name(),
                Some("registry.tilo.so/tilo/tilo-vm:x86_64-ci"),
                "and is named by its image"
            );
            assert_eq!(deployments.rollback, None, "nothing to go back to");
        }

        phase a_failed_listing_keeps_its_status;
        {
            let (_dir, path, server) = serve_once("1\ntilo-deployment: no ESP\n");
            let answer = ask(&path, Listing::Text).unwrap();
            server.join().unwrap();
            assert_eq!(answer.code, 1);
            assert_eq!(answer.text, "tilo-deployment: no ESP\n");

            let (_dir, path, server) = serve_once("1\nbootc: busy\n");
            let error = read(&path).unwrap_err();
            server.join().unwrap();
            assert!(matches!(error, AskError::Failed(Answer { code: 1, .. })), "{error}");
        }

        phase a_service_that_dies_without_an_answer_is_an_error;
        {
            let (_dir, path, server) = serve_once("");
            let error = ask(&path, Listing::Text).unwrap_err();
            server.join().unwrap();
            assert!(matches!(error, AskError::NoAnswer), "{error}");
        }

        phase a_socket_only_admins_may_reach_says_so;
        {
            use std::os::unix::fs::PermissionsExt as _;
            let (_dir, path, server) = serve_once("0\n{}\n");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
            let answer = ask(&path, Listing::Json);
            // Root reaches any socket; everyone else is refused as the service refuses a
            // member who is not an admin.
            if let Err(error) = answer {
                assert!(matches!(error, AskError::NotAnAdmin), "{error}");
                drop(server);
            } else {
                server.join().unwrap();
            }
        }
    }
}
