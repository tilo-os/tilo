//! Bind descriptor custody to the actual app without changing launch authority.
use super::{CollabLaunchHandoff, HostServices, native_media::PeerProcess};
use tokio::io::AsyncWriteExt as _;

pub(super) async fn bind(
    stream: &mut tokio::net::UnixStream,
    launcher: Option<PeerProcess>,
    services: &HostServices,
    pid: u32,
    handoff: &CollabLaunchHandoff,
) -> Result<PeerProcess, capnp::Error> {
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        bind_inner(stream, launcher, services, pid, handoff),
    )
    .await
    .map_err(|_| capnp::Error::failed("native executing-task binding timed out".into()))?
    .map_err(|error| {
        capnp::Error::failed(format!("native executing-task binding refused: {error}"))
    })
}
async fn bind_inner(
    stream: &mut tokio::net::UnixStream,
    launcher: Option<PeerProcess>,
    services: &HostServices,
    pid: u32,
    handoff: &CollabLaunchHandoff,
) -> std::io::Result<PeerProcess> {
    let refused = || {
        std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "original launch or executing task changed",
        )
    };
    let launcher = launcher.filter(PeerProcess::alive).ok_or_else(refused)?;
    let authority = services.launcher.as_ref().ok_or_else(refused)?;
    runtime_portal::process_binding::arm(stream)?;
    let mut challenge = [0; 32];
    challenge[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    challenge[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    stream
        .write_all(&runtime_portal::process_binding::MAGIC)
        .await?;
    stream.write_all(&challenge).await?;
    // SO_PASSCRED was armed before the challenge; every chunk is tied to the
    // same sender. Cancellation drops its one optional proof descriptor.
    let mut receiver =
        runtime_portal::process_binding::Receiver::new(challenge, unsafe { libc::geteuid() });
    let peer = loop {
        if let Some(peer) = stream
            .async_io(tokio::io::Interest::READABLE, || receiver.receive(stream))
            .await?
        {
            break peer;
        }
    };
    if !launcher.alive()
        || !authority.attest_executing_process(pid, &peer, &handoff.app_id, &handoff.workspace_id)
    {
        return Err(refused());
    }
    let process = launcher.with_executing(peer).ok_or_else(refused)?;
    stream.write_all(&[1]).await?;
    Ok(process)
}
