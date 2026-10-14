//! Owner-authenticated local transport gate used by the mandatory partition
//! release harness. It is disabled unless the daemon is explicitly launched
//! with `TILO_FS_TRANSPORT_GATE_SOCKET`.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;

pub const TRANSPORT_GATE_SOCKET_ENV: &str = "TILO_FS_TRANSPORT_GATE_SOCKET";
pub const TRANSPORT_GATE_INITIAL_ENV: &str = "TILO_FS_TRANSPORT_GATE_INITIAL";

#[derive(Clone, Debug, Default)]
pub struct TransportGate(Arc<AtomicBool>);

impl TransportGate {
    pub fn partitioned(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    pub fn set_partitioned(&self, value: bool) {
        self.0.store(value, Ordering::Release);
    }
}

/// Start the optional owner-authenticated partition gate configured by the environment.
///
/// # Errors
///
/// Returns an error when the socket path is unsafe or cannot be bound securely.
pub fn spawn_from_env() -> std::io::Result<(TransportGate, Option<tokio::task::JoinHandle<()>>)> {
    let gate = TransportGate::default();
    let Some(path) = std::env::var_os(TRANSPORT_GATE_SOCKET_ENV).map(PathBuf::from) else {
        return Ok((gate, None));
    };
    if std::env::var_os(TRANSPORT_GATE_INITIAL_ENV).is_some_and(|value| value == "partitioned") {
        gate.set_partitioned(true);
    }
    let task = spawn(path, gate.clone())?;
    Ok((gate, Some(task)))
}

fn spawn(path: PathBuf, gate: TransportGate) -> std::io::Result<tokio::task::JoinHandle<()>> {
    validate_private_parent(&path)?;
    let listener = tilo_files::native::private::bind_socket(&path)?;
    listener.set_nonblocking(true)?;
    let listener = UnixListener::from_std(listener)?;
    println!("Transport partition gate at {}", path.display());
    Ok(tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let Ok(credential) = stream.peer_cred() else {
                continue;
            };
            if credential.uid() != rustix::process::geteuid().as_raw() {
                continue;
            }
            let mut command = [0_u8; 32];
            let Ok(size) = stream.read(&mut command).await else {
                continue;
            };
            let response = match command.get(..size).unwrap_or_default() {
                b"partition\n" => {
                    gate.set_partitioned(true);
                    b"partitioned\n".as_slice()
                }
                b"reconnect\n" => {
                    gate.set_partitioned(false);
                    b"connected\n".as_slice()
                }
                b"status\n" if gate.partitioned() => b"partitioned\n".as_slice(),
                b"status\n" => b"connected\n".as_slice(),
                _ => b"invalid\n".as_slice(),
            };
            let _ = stream.write_all(response).await;
            let _ = stream.shutdown().await;
        }
        let _ = std::fs::remove_file(path);
    }))
}

fn validate_private_parent(path: &Path) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("gate has no parent"))?;
    let metadata = std::fs::metadata(parent)?;
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "transport gate parent must be owner-only",
        ));
    }
    Ok(())
}
