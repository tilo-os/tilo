//! First enrollment stays in private RAM before any account/key/grant exists.
//! Only actual signed namespace completion can produce an importable snapshot.
use super::super::NativeMessagingIdentity;
use super::{Arc, File, NativeSignalAccount, RamDirectory, SendError, io};
use launcher::owned_service::{ServiceAuthority, ServiceLeaseKey};
use std::{path::Path, sync::mpsc, time::Duration};

pub(in crate::shell::in_process::messaging) struct Provisioning {
    pub stream: std::os::unix::net::UnixStream,
    pub stopped: mpsc::Receiver<Result<UnpublishedSignalProfile, SendError>>,
}
pub(in crate::shell::in_process::messaging) struct UnpublishedSignalProfile {
    original: NativeMessagingIdentity,
    files: super::collection::Files,
}
impl UnpublishedSignalProfile {
    /// A synthetic peer has no provider process or profile writer. This does
    /// not construct or bypass a launcher's actual namespace-exit witness.
    #[cfg(test)]
    pub(in crate::shell::in_process::messaging) fn writer_free_fixture(
        original: NativeMessagingIdentity,
    ) -> Self {
        Self {
            original,
            files: std::collections::BTreeMap::from([(
                "data/account".into(),
                zeroize::Zeroizing::new(b"synthetic-linked-profile".to_vec()),
            )]),
        }
    }

    pub(in crate::shell::in_process::messaging) fn seal(
        self,
        account: &NativeSignalAccount,
        root: &Path,
    ) -> Result<(), SendError> {
        self.original.check()?;
        account.gate.check()?;
        if self.original.owner != account.gate.startup.identity.owner {
            return Err(SendError::Revoked);
        }
        let mut sealed = account.create_profile(root)?;
        let image = sealed.begin_use().map_err(|_| SendError::Unavailable)?;
        sealed
            .checkpoint_files(image, self.files)
            .map_err(|_| SendError::Unavailable)
    }
}
struct ProvisioningAuthority(NativeMessagingIdentity, Arc<super::super::network::Consent>);
impl ServiceAuthority for ProvisioningAuthority {
    fn network_hosts(&self) -> io::Result<Vec<String>> {
        self.1.hosts()
    }
    fn check(&self) -> io::Result<()> {
        self.1
            .check()
            .map_err(|_| io::ErrorKind::PermissionDenied)?;
        self.0
            .check()
            .map_err(|_| io::ErrorKind::PermissionDenied.into())
    }
}
pub(in crate::shell::in_process::messaging) fn provision(
    original: NativeMessagingIdentity,
    runtime_root: &Path,
    policy: &super::super::network::Policy,
) -> Result<Provisioning, SendError> {
    original.check()?;
    let consent = super::super::network::request(original.clone(), policy)?;
    let cancellation = &policy.cancellation;
    let ram = RamDirectory::allocate(runtime_root).map_err(|_| SendError::Unavailable)?;
    // Materialize the empty app subtree, never another account's directory.
    let mut directory: File = ram.root.try_clone().map_err(|_| SendError::Unavailable)?;
    for part in [
        launcher::appdir::ROOMS_DIR,
        launcher::appdir::PERSONAL_ROOM,
        super::SERVICE_PACKAGE,
    ] {
        directory = super::child_directory(
            &directory,
            &super::component(std::ffi::OsStr::new(part)).map_err(|_| SendError::Unavailable)?,
        )
        .map_err(|_| SendError::Unavailable)?;
    }
    drop(directory);
    let mut launch = super::super::provider::acquire_launch(|| original.check(), cancellation)?;
    launch.options.data_home = Some(ram.source());
    let key = ServiceLeaseKey::default();
    let (streams, receive) = mpsc::sync_channel(1);
    let (completed, stopped) = mpsc::sync_channel(1);
    let authority = Arc::new(ProvisioningAuthority(original.clone(), consent));
    let service = cancellation.clone();
    let runtime = crate::session_runtime::runtime().map_err(|_| SendError::Unavailable)?;
    drop(runtime.spawn_blocking(move || {
        let result = (|| {
            let witness = launcher::owned_service::run_connected(
                launch,
                authority,
                service,
                key.clone(),
                Some(Box::new(move |endpoint| {
                    let [first, second] = super::super::provider::streams(&endpoint)?;
                    drop(second);
                    streams
                        .send(first)
                        .map_err(|_| io::ErrorKind::Interrupted.into())
                })),
            )
            .map_err(|_| SendError::Unavailable)?;
            if !witness.matches(&key, &ram.source(), super::SERVICE_PACKAGE) {
                return Err(SendError::Revoked);
            }
            let files = ram.collect_profile().map_err(|_| SendError::Unavailable)?;
            original.check()?;
            Ok(UnpublishedSignalProfile { original, files })
        })();
        let _ = completed.send(result);
    }));
    let stream = receive.recv_timeout(Duration::from_secs(25)).map_err(|_| {
        cancellation.cancel();
        SendError::Unavailable
    })?;
    Ok(Provisioning { stream, stopped })
}
