//! Native capture launcher custody. Frontends retain this handle, never a
//! launcher authority implementation or a freely assembled service launch.
use launcher::{
    owned_service::{
        self, CaptureAdmission, CaptureOutputAdmission, ConnectedService, QuiescentExit,
        ServiceAuthority, ServiceCancellation, ServiceLaunch, ServiceLeaseKey,
    },
    run::{IdentitySource, LaunchMode, RunError, RunOptions},
};
use std::{fs::File, io, path::Path, sync::Arc};

#[cfg(test)]
mod tests;

const PACKAGE: &str = "so.tilo.imager.home-export";

struct Authority(ServiceCancellation);
impl ServiceAuthority for Authority {
    fn check(&self) -> io::Result<()> {
        if self.0.is_cancelled() {
            Err(io::ErrorKind::Interrupted.into())
        } else {
            Ok(())
        }
    }
}

/// Owns the verified source, runtime, output and signed installation through
/// canonical namespace teardown. No API exports or accepts `ServiceAuthority`.
pub struct CaptureHostOwner {
    admission: CaptureAdmission,
    installation: Option<Installation>,
    cancellation: ServiceCancellation,
    key: ServiceLeaseKey,
}
struct Installation {
    manifest: std::path::PathBuf,
    payload: std::path::PathBuf,
    package_hash: String,
}
impl CaptureHostOwner {
    /// Verify actual native source custody and the independently pinned runtime.
    /// Serialized source labels alone cannot create this owner. The publisher
    /// must come from the host's embedded signed-service policy, as before.
    pub fn verify(runtime: &Path, publisher: &str, source: File, uid: u32) -> io::Result<Self> {
        let actual = rustix::process::getuid().as_raw();
        if uid == 0 || actual != uid || rustix::process::geteuid().as_raw() != actual {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let admission =
            CaptureAdmission::verify(runtime, publisher, source, uid)?.with_archive_helper()?;
        Ok(Self {
            admission,
            installation: None,
            cancellation: ServiceCancellation::default(),
            key: ServiceLeaseKey::default(),
        })
    }

    /// Admit the exact verified signed worker, retaining the same package hash
    /// and paths for launch rather than accepting a second frontend launch DTO.
    pub fn with_installation(
        mut self,
        installed: package_store::InstalledPackage,
        publisher: &str,
    ) -> io::Result<Self> {
        if self.installation.is_some() || installed.id != PACKAGE {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let installation = Installation {
            manifest: installed.manifest_path.clone(),
            payload: installed.payload_root.clone(),
            package_hash: installed.package_id_hex.clone(),
        };
        self.admission = self.admission.with_installation(installed, publisher)?;
        self.installation = Some(installation);
        Ok(self)
    }

    pub fn with_inventory_scratch(mut self, scratch: File, quota: u64) -> io::Result<Self> {
        self.admission = self.admission.with_inventory_scratch(scratch, quota)?;
        Ok(self)
    }

    pub fn with_broker_output(
        mut self,
        output: File,
        proof: CaptureOutputAdmission,
    ) -> io::Result<Self> {
        self.admission = self.admission.with_broker_output(output, proof)?;
        Ok(self)
    }

    /// Cancellation is a revocation-only handle, not a grant constructor.
    pub fn cancellation(&self) -> ServiceCancellation {
        self.cancellation.clone()
    }

    /// Correlate the eventual canonical quiescence witness with this launch.
    pub fn lease_key(&self) -> ServiceLeaseKey {
        self.key.clone()
    }

    /// Launch only the admitted fixed worker with offline service policy. The
    /// canonical launcher still checks source owner, package and namespace
    /// admission before effects, and retains custody until quiescence.
    pub fn run(
        self,
        working: &Path,
        connected: Option<ConnectedService>,
    ) -> Result<QuiescentExit, RunError> {
        let installation = self.installation.ok_or_else(|| {
            RunError::InvalidServiceLaunch("Capture installation was not admitted".into())
        })?;
        let launch = ServiceLaunch {
            manifest: installation.manifest,
            payload: installation.payload,
            options: RunOptions {
                no_services: true,
                expected_package_hash: Some(installation.package_hash),
                app_id: Some(PACKAGE.into()),
                data_home: Some(working.to_path_buf()),
                mode: LaunchMode::Service {
                    passthrough_argv: Vec::new(),
                },
                identity: IdentitySource::None,
                ..RunOptions::default()
            },
        };
        let authority = Arc::new(Authority(self.cancellation.clone()));
        owned_service::run_capture_connected(
            launch,
            self.admission,
            authority,
            self.cancellation,
            self.key,
            connected,
        )
    }
}
