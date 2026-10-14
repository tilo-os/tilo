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
    // Include diagnostic observation time in the existing binding budget.
    let deadline = tokio::time::Instant::now()
        .checked_add(std::time::Duration::from_secs(10))
        .ok_or_else(|| {
            capnp::Error::failed("native executing-task binding deadline unavailable".into())
        })?;
    let observation = Observation {
        services,
        id: services
            .launcher
            .as_ref()
            .and_then(|authority| authority.begin_executing_binding_observation(pid)),
    };
    observation.phase(Phase::Started);
    let result = tokio::time::timeout_at(
        deadline,
        bind_inner(stream, launcher, &observation, pid, handoff),
    )
    .await;
    match result {
        Ok(Ok(process)) => {
            observation.phase(Phase::Admitted);
            Ok(process)
        }
        Ok(Err(error)) => {
            observation.phase(Phase::Refused);
            Err(capnp::Error::failed(format!(
                "native executing-task binding refused: {error}"
            )))
        }
        Err(_) => {
            observation.phase(Phase::TimedOut);
            Err(capnp::Error::failed(
                "native executing-task binding timed out".into(),
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
    reason = "in these kernel-socket fixtures a failed unwrap is the test assertion"
)]
mod tests {
    use super::*;
    use crate::services::{LaunchableApplication, LauncherServices};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Observer(Mutex<Vec<(u64, Phase)>>);
    impl LauncherServices for Observer {
        fn begin_executing_binding_observation(&self, _pid: u32) -> Option<u64> {
            Some(7)
        }
        fn observe_executing_binding(&self, id: u64, phase: Phase) {
            self.0.lock().unwrap().push((id, phase));
        }
        fn applications(&self) -> Vec<LaunchableApplication> {
            Vec::new()
        }
        fn launch(
            &self,
            _app: &str,
            _workspace: &tilo_desktop_model::Workspace,
            _record: Option<&str>,
            _place: &crate::services::LaunchPlace,
        ) -> Result<(), String> {
            Err("not a launching fixture".into())
        }
        fn attest_process(&self, _pid: u32, _app: &str, _room: &str) -> bool {
            false
        }
    }

    fn handoff() -> CollabLaunchHandoff {
        CollabLaunchHandoff {
            protocol: 1,
            app_id: "dev.tilo.binding-test".into(),
            workspace_id: "fixture-room".into(),
            executing_peer: true,
        }
    }

    #[tokio::test]
    async fn binding_observation_does_not_admit_a_missing_original_peer() {
        let observer = Arc::new(Observer::default());
        let services = HostServices {
            launcher: Some(observer.clone()),
            ..HostServices::default()
        };
        let (mut server, _client) = tokio::net::UnixStream::pair().unwrap();
        assert!(
            bind(&mut server, None, &services, std::process::id(), &handoff())
                .await
                .is_err()
        );
        assert_eq!(
            *observer.0.lock().unwrap(),
            [(7, Phase::Started), (7, Phase::Refused)]
        );
    }

    #[tokio::test]
    async fn binding_timeout_retains_its_original_observation_without_extending_the_deadline() {
        let observer = Arc::new(Observer::default());
        let services = HostServices {
            launcher: Some(observer.clone()),
            ..HostServices::default()
        };
        let (mut server, _client) = tokio::net::UnixStream::pair().unwrap();
        let launcher = PeerProcess::from_socket(&server).expect("real kernel peer pidfd");
        let result = bind(
            &mut server,
            Some(launcher),
            &services,
            std::process::id(),
            &handoff(),
        )
        .await;
        assert!(matches!(result, Err(error) if error.to_string().contains("binding timed out")));
        assert_eq!(
            *observer.0.lock().unwrap(),
            [
                (7, Phase::Started),
                (7, Phase::Challenge),
                (7, Phase::TimedOut)
            ]
        );
    }
}
