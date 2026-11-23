//! The person's terminal: how `tilo-install` and `app` change the package store while the device
//! runs. They write nothing themselves; every change is the service's [`Change`] action, so it gets
//! the service's permissions and its record of who changed what, as the Store's changes do.

use std::io::Read as _;
use std::path::Path;

use crate::client::BlockingClient;
use crate::wire::{Change, ChangeReport};

/// The package store the person's own processes mean: what the service they reach must write.
///
/// # Errors
///
/// With no data directory, a sentence saying so.
pub fn store_root() -> Result<std::path::PathBuf, String> {
    package_store::layout::default_user_root()
        .ok_or_else(|| "no data directory (HOME or XDG_DATA_HOME) for the package store".to_owned())
}

/// Make `change` through the package service, uploading `file` first when it names one.
///
/// # Errors
///
/// The service's refusal or failure, or a sentence saying why it could not be reached.
pub fn change(change: Change, file: Option<&Path>) -> Result<ChangeReport, String> {
    let store = store_root()?;
    connect()?.change(&store, change, file)
}

/// Show what a change did: its warnings on standard error after `program: `, its lines on
/// standard output.
pub fn show(report: &ChangeReport, program: &str) {
    for warning in &report.warnings {
        eprintln!("{program}: {warning}");
    }
    for line in &report.lines {
        println!("{line}");
    }
}

/// Connect to the person's package service, starting its socket when this login has not: a
/// Tilo session starts it, and another desktop's login starts it here on first use.
///
/// # Errors
///
/// A sentence naming the socket and what to do when no service answers there.
pub fn connect() -> Result<BlockingClient, String> {
    let socket = crate::default_socket_path().ok_or(
        "no runtime directory (XDG_RUNTIME_DIR) to reach the package service through: \
         changes to the package store go through it",
    )?;
    let mut stream = match std::os::unix::net::UnixStream::connect(&socket) {
        Ok(stream) => stream,
        Err(first) => {
            let started = std::process::Command::new("systemctl")
                .args(["--user", "start", "tilo-package-service.socket"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            match std::os::unix::net::UnixStream::connect(&socket) {
                Ok(stream) if started => stream,
                _ => {
                    return Err(format!(
                        "no package service answers at {} ({first}), and none could be \
                         started (`systemctl --user start tilo-package-service.socket`): \
                         changes to the package store go through it. An image build writes \
                         its store with `--offline-store DIR` instead",
                        socket.display()
                    ));
                }
            }
        }
    };
    crate::wire::write_handoff(&mut stream, crate::wire::PERSON)
        .map_err(|error| format!("package service at {}: {error}", socket.display()))?;
    BlockingClient::from_connected_fd(stream.into())
}

impl BlockingClient {
    /// Make `change` to the store at `store`, uploading `file` first when it names one.
    ///
    /// # Errors
    ///
    /// The service's refusal or failure, or an unreadable `file`.
    pub fn change(
        &self,
        store: &Path,
        change: Change,
        file: Option<&Path>,
    ) -> Result<ChangeReport, String> {
        use crate::wire::{Action, Answer, MAX_UPLOAD_BYTES, UPLOAD_CHUNK};
        if let Some(path) = file {
            let mut reader = std::fs::File::open(path)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            let size = reader
                .metadata()
                .map_err(|error| format!("{}: {error}", path.display()))?
                .len();
            if size > MAX_UPLOAD_BYTES {
                return Err(format!(
                    "{}: {size} bytes is more than a package may be ({MAX_UPLOAD_BYTES})",
                    path.display()
                ));
            }
            loop {
                let mut chunk = Vec::new();
                (&mut reader)
                    .take(UPLOAD_CHUNK)
                    .read_to_end(&mut chunk)
                    .map_err(|error| format!("{}: {error}", path.display()))?;
                if chunk.is_empty() {
                    break;
                }
                let last = (chunk.len() as u64) < UPLOAD_CHUNK;
                self.invoke(Action::Upload { chunk })?;
                if last {
                    break;
                }
            }
        }
        match self.invoke(Action::Change {
            store: store.display().to_string(),
            change,
        })? {
            Answer::Report(report) => Ok(report),
            other => Err(format!("package-service answered {other:?}")),
        }
    }
}
