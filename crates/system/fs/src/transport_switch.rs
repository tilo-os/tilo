//! Owner-authenticated local transport check for the partition release harness,
//! enabled by `TILO_FS_TRANSPORT_SWITCH_SOCKET`.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;

pub const TRANSPORT_SWITCH_SOCKET_ENV: &str = "TILO_FS_TRANSPORT_SWITCH_SOCKET";
pub const TRANSPORT_SWITCH_INITIAL_ENV: &str = "TILO_FS_TRANSPORT_SWITCH_INITIAL";

#[derive(Clone, Debug, Default)]
pub struct TransportSwitch(Arc<AtomicBool>);

impl TransportSwitch {
    pub fn partitioned(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    pub fn set_partitioned(&self, value: bool) {
        self.0.store(value, Ordering::Release);
    }
}

pub fn spawn_from_env() -> std::io::Result<(TransportSwitch, Option<tokio::task::JoinHandle<()>>)> {
    let check = TransportSwitch::default();
    let Some(path) = std::env::var_os(TRANSPORT_SWITCH_SOCKET_ENV).map(PathBuf::from) else {
        return Ok((check, None));
    };
    if std::env::var_os(TRANSPORT_SWITCH_INITIAL_ENV).is_some_and(|value| value == "partitioned") {
        check.set_partitioned(true);
    }
    let task = spawn(path, check.clone())?;
    Ok((check, Some(task)))
}

fn spawn(path: PathBuf, check: TransportSwitch) -> std::io::Result<tokio::task::JoinHandle<()>> {
    validate_private_parent(&path)?;
    let listener = tilo_files::native::private::bind_socket(&path)?;
    listener.set_nonblocking(true)?;
    let listener = UnixListener::from_std(listener)?;
    println!("Transport partition check at {}", path.display());
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
                    check.set_partitioned(true);
                    b"partitioned\n".as_slice()
                }
                b"reconnect\n" => {
                    check.set_partitioned(false);
                    b"connected\n".as_slice()
                }
                b"status\n" if check.partitioned() => b"partitioned\n".as_slice(),
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
        .ok_or_else(|| std::io::Error::other("check has no parent"))?;
    let metadata = std::fs::metadata(parent)?;
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "transport check parent must be owner-only",
        ));
    }
    Ok(())
}
