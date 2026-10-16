//! Bind descriptor custody to the actual app without changing launch authority.
use super::{CollabLaunchHandoff, HostServices, native_media::PeerProcess};
use crate::services::ExecutingBindingPhase as Phase;
use tokio::io::AsyncWriteExt as _;

struct Observation<'a> {
    services: &'a HostServices,
    id: Option<u64>,
}
impl Observation<'_> {
    fn phase(&self, phase: Phase) {
        if let (Some(launcher), Some(id)) = (&self.services.launcher, self.id) {
            launcher.observe_executing_binding(id, phase);
        }
    }
}

pub(super) async fn bind(
    stream: &mut tokio::net::UnixStream,
    launcher: Option<PeerProcess>,
    services: &HostServices,
    pid: u32,
    handoff: &CollabLaunchHandoff,
) -> Result<PeerProcess, capnp::Error> {
    let observation = Observation {
        services,
        id: services
            .launcher
            .as_ref()
            .and_then(|authority| authority.begin_executing_binding_observation(pid)),
    };
    observation.phase(Phase::Started);
    // The app answers once it has started: its sandbox, D-Bus proxy and
    // executable are set up first, and on a loaded machine that takes as
    // long as it takes. The binding therefore waits for the answer, not for
    // a clock. It ends when the answer is refused, when the app closes the
    // connection, or when the launch it belongs to has exited.
    let launch = launcher.clone();
    tokio::select! {
        result = bind_inner(stream, launcher, &observation, pid, handoff) => match result {
            Ok(process) => {
                observation.phase(Phase::Admitted);
                Ok(process)
            }
            Err(error) => {
                observation.phase(Phase::Refused);
                let reason = if error.kind() == std::io::ErrorKind::UnexpectedEof {
                    "the application closed the connection before it answered".to_string()
                } else {
                    format!("refused: {error}")
                };
                Err(capnp::Error::failed(format!("native executing-task binding {reason}")))
            }
        },
        () = async {
            match &launch {
                Some(launch) => launch.exited().await,
                None => std::future::pending::<()>().await,
            }
        } => {
            observation.phase(Phase::Abandoned);
            Err(capnp::Error::failed(
                "native executing-task binding abandoned: the launch exited before the application answered".into(),
            ))
        }
    }
}
async fn bind_inner(
    stream: &mut tokio::net::UnixStream,
    launcher: Option<PeerProcess>,
    observation: &Observation<'_>,
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
    let authority = observation.services.launcher.as_ref().ok_or_else(refused)?;
    runtime_portal::process_binding::arm(stream)?;
    let mut challenge = [0; 32];
    challenge[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    challenge[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    stream
        .write_all(&runtime_portal::process_binding::MAGIC)
        .await?;
    stream.write_all(&challenge).await?;
    observation.phase(Phase::Challenge);
    // SO_PASSCRED was armed before the challenge; every chunk is tied to the
    // same sender. Cancellation drops its one optional proof descriptor.
    let mut receiver = runtime_portal::process_binding::Receiver::new(
        challenge,
        rustix::process::geteuid().as_raw(),
    );
    let peer = loop {
        if let Some(peer) = stream
            .async_io(tokio::io::Interest::READABLE, || receiver.receive(stream))
            .await?
        {
            break peer;
        }
    };
    observation.phase(Phase::Answer);
    if !launcher.alive()
        || !authority.attest_executing_process(pid, &peer, &handoff.app_id, &handoff.workspace_id)
    {
        return Err(refused());
    }
    let process = launcher.with_executing(peer).ok_or_else(refused)?;
    stream.write_all(&[1]).await?;
    Ok(process)
}

#[cfg(all(test, target_os = "linux"))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in these kernel-socket fixtures a failed unwrap is the test assertion"
)]
mod tests {
    use super::*;
    use crate::services::LauncherServices;
    use std::sync::Arc;

    struct Attests;
    impl LauncherServices for Attests {
        fn applications(&self) -> Vec<crate::services::LaunchableApplication> {
            Vec::new()
        }
        fn launch(
            &self,
            _: &str,
            _: &tilo_desktop_model::Workspace,
            _: Option<&str>,
            _: &crate::services::LaunchPlace,
        ) -> Result<(), String> {
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

    #[derive(Default)]
    struct Observer(std::sync::Mutex<Vec<(u64, Phase)>>);
    impl LauncherServices for Observer {
        fn begin_executing_binding_observation(&self, _pid: u32) -> Option<u64> {
            Some(7)
        }
        fn observe_executing_binding(&self, id: u64, phase: Phase) {
            self.0.lock().expect("observations").push((id, phase));
        }
        fn applications(&self) -> Vec<crate::services::LaunchableApplication> {
            Vec::new()
        }
        fn launch(
            &self,
            _: &str,
            _: &tilo_desktop_model::Workspace,
            _: Option<&str>,
            _: &crate::services::LaunchPlace,
        ) -> Result<(), String> {
            Err("not a launching fixture".into())
        }
        fn attest_process(&self, _: u32, _: &str, _: &str) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn binding_observation_does_not_admit_a_missing_original_peer() {
        let observer = Arc::new(Observer::default());
        let services = HostServices {
            launcher: Some(observer.clone()),
            ..HostServices::default()
        };
        let (mut server, _client) = tokio::net::UnixStream::pair().expect("socket pair");
        assert!(
            bind(&mut server, None, &services, std::process::id(), &handoff())
                .await
                .is_err()
        );
        assert_eq!(
            *observer.0.lock().expect("observations"),
            [(7, Phase::Started), (7, Phase::Refused)]
        );
    }

    /// An app that goes away mid-handshake is observed through the challenge
    /// to its refusal, never as admitted.
    #[tokio::test]
    async fn binding_observation_follows_a_departed_app_to_its_refusal() {
        let (mut host, mut app, launcher) = connection();
        let observer = Arc::new(Observer::default());
        let services = HostServices {
            launcher: Some(observer.clone()),
            ..HostServices::default()
        };
        let leaving = std::thread::spawn(move || {
            use std::io::Read as _;
            let mut challenge = [0; 36];
            app.read_exact(&mut challenge)
                .expect("the host's challenge");
        });
        let bound = bind(
            &mut host,
            Some(launcher),
            &services,
            std::process::id(),
            &handoff(),
        )
        .await;
        leaving.join().expect("app thread");
        assert!(bound.is_err());
        assert_eq!(
            *observer.0.lock().expect("observations"),
            [
                (7, Phase::Started),
                (7, Phase::Challenge),
                (7, Phase::Refused)
            ]
        );
    }
}
