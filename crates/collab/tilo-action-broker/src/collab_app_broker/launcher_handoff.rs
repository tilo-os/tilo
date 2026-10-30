//! A launcher's handoff on a new connection: the marker, the kernel's
//! credentials for the process behind it, and the launch it names.
use super::{CollabLaunchHandoff, has_collab_launch_marker, native_media};
use crate::BrokerError;
use tokio::io::AsyncReadExt;

/// A launcher's handoff, read whole: the stream is now where the app's
/// first wire frame begins.
pub(super) struct LauncherHandoff {
    pub handoff: CollabLaunchHandoff,
    pub pid: u32,
    pub media_process: Option<native_media::PeerProcess>,
}

/// Read the launcher's marker and handoff off `stream`; `None` for a raw
/// client that sent no marker.
pub(super) async fn read_launcher_handoff(
    stream: &mut tokio::net::UnixStream,
) -> Result<Option<LauncherHandoff>, BrokerError> {
    if !has_collab_launch_marker(stream).await? {
        return Ok(None);
    }
    let credentials = stream.peer_cred().map_err(|error| {
        BrokerError::failed(format!(
            "could not read collaboration peer credentials: {error}"
        ))
    })?;
    // A socket pathname is not authority. The kernel must identify a process
    // from this user before it can attest the compositor's launch record.
    if credentials.uid() != rustix::process::geteuid().as_raw() {
        return Err(BrokerError::failed(
            "collaboration service rejected a peer from another user".into(),
        ));
    }
    let pid = credentials
        .pid()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0)
        .ok_or_else(|| BrokerError::failed("collaboration peer has no usable pid".into()))?;
    // Retain this exact connected task before consulting the expiring launch
    // record. Unsupported kernels keep ordinary collaboration available.
    let media_process = native_media::PeerProcess::from_socket(stream);

    let mut marker = [0_u8; launcher::services::COLLAB_LAUNCH_MAGIC.len()];
    stream.read_exact(&mut marker).await.map_err(|error| {
        BrokerError::failed(format!(
            "could not read collaboration launch marker: {error}"
        ))
    })?;
    if marker != launcher::services::COLLAB_LAUNCH_MAGIC {
        return Err(BrokerError::failed(
            "collaboration launch marker changed while being read".into(),
        ));
    }
    let bytes = match launcher::services::COLLAB_LAUNCH_FRAMING
        .read_async(stream)
        .await
    {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            return Err(BrokerError::failed(
                "collaboration launch handoff is missing".into(),
            ));
        }
        Err(wire_frame::FrameError::EmptyFrame | wire_frame::FrameError::TooLarge(_)) => {
            return Err(BrokerError::failed(
                "collaboration launch handoff has an invalid length".into(),
            ));
        }
        Err(error) => {
            return Err(BrokerError::failed(format!(
                "could not read collaboration launch handoff: {error}"
            )));
        }
    };
    let handoff: CollabLaunchHandoff = serde_json::from_slice(&bytes).map_err(|error| {
        BrokerError::failed(format!("invalid collaboration launch handoff: {error}"))
    })?;
    Ok(Some(LauncherHandoff {
        handoff,
        pid,
        media_process,
    }))
}

/// The peer on `stream` as its kernel credentials name it, before any
/// handoff says who it is.
pub(super) fn credentialed_peer(stream: &tokio::net::UnixStream) -> String {
    stream.peer_cred().ok().and_then(|c| c.pid()).map_or_else(
        || "a peer without credentials".to_owned(),
        |pid| format!("pid {pid}"),
    )
}
