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
    // The app answers once it has started: its sandbox, D-Bus proxy and
    // executable are set up first, and on a loaded machine that takes as
    // long as it takes. The binding therefore waits for the answer, not for
    // a clock. It ends when the answer is refused, when the app closes the
    // connection, or when the launch it belongs to has exited.
    let launch = launcher.clone();
    tokio::select! {
        result = bind_inner(stream, launcher, services, pid, handoff) => result.map_err(|error| {
            let reason = if error.kind() == std::io::ErrorKind::UnexpectedEof {
                "the application closed the connection before it answered".to_string()
            } else {
                format!("refused: {error}")
            };
            capnp::Error::failed(format!("native executing-task binding {reason}"))
        }),
        () = async {
            match &launch {
                Some(launch) => launch.exited().await,
                None => std::future::pending::<()>().await,
            }
        } => Err(capnp::Error::failed(
            "native executing-task binding abandoned: the launch exited before the application answered".into(),
        )),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::LauncherServices;
    use std::sync::Arc;

    struct Attests;
    impl LauncherServices for Attests {
        fn applications(&self) -> Vec<crate::services::LaunchableApplication> {
            Vec::new()
        }
        fn launch(&self, _: &str, _: &tilo_desktop_model::Workspace) -> Result<(), String> {
            Err("not under test".into())
        }
        fn attest_process(&self, _: u32, _: &str, _: &str) -> bool {
            true
        }
        fn attest_executing_process(
            &self,
            _: u32,
            _: &runtime_portal::process_binding::ExecutingPeer,
            _: &str,
            _: &str,
        ) -> bool {
            true
        }
    }

    fn handoff() -> CollabLaunchHandoff {
        CollabLaunchHandoff {
            protocol: 1,
            app_id: "so.tilo.calculator".into(),
            workspace_id: "room".into(),
            executing_peer: true,
        }
    }

    fn connection() -> (
        tokio::net::UnixStream,
        std::os::unix::net::UnixStream,
        PeerProcess,
    ) {
        let (host, app) = std::os::unix::net::UnixStream::pair().expect("service socket pair");
        host.set_nonblocking(true).expect("async host end");
        let host = tokio::net::UnixStream::from_std(host).expect("host transport");
        let launcher = PeerProcess::from_socket(&host).expect("kernel launch pidfd");
        (host, app, launcher)
    }

    /// The app answers only once its sandbox and runtime have started, which
    /// on a loaded machine outlasts any fixed wait. The clock is paused and
    /// jumps whenever the runtime idles, so a deadline in `bind` would fire
    /// here while the app is still starting; the binding must wait for the
    /// answer instead.
    #[tokio::test(start_paused = true)]
    async fn a_slow_starting_app_is_still_bound() {
        let (mut host, mut app, launcher) = connection();
        let services = HostServices {
            launcher: Some(Arc::new(Attests)),
            ..HostServices::default()
        };
        let starting = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(400));
            runtime_portal::process_binding::complete(&mut app)
        });
        let bound = bind(
            &mut host,
            Some(launcher),
            &services,
            std::process::id(),
            &handoff(),
        )
        .await;
        starting
            .join()
            .expect("app thread")
            .expect("the app completes its side of the binding");
        bound.expect("a late answer binds, it does not time out");
    }

    /// A genuine failure keeps its typed, user-visible reason.
    #[tokio::test]
    async fn an_app_that_goes_away_is_reported_as_such() {
        let (mut host, mut app, launcher) = connection();
        let services = HostServices {
            launcher: Some(Arc::new(Attests)),
            ..HostServices::default()
        };
        // It reads the challenge, then goes away without answering.
        let leaving = std::thread::spawn(move || {
            use std::io::Read as _;
            let mut challenge = [0; 36];
            app.read_exact(&mut challenge)
                .expect("the host's challenge");
        });
        let Err(error) = bind(
            &mut host,
            Some(launcher),
            &services,
            std::process::id(),
            &handoff(),
        )
        .await
        else {
            panic!("a closed connection cannot bind");
        };
        leaving.join().expect("app thread");
        assert!(
            error
                .to_string()
                .contains("closed the connection before it answered"),
            "{error}"
        );
    }
}
