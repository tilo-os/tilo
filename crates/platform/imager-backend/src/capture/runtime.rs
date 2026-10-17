//! Native capture admission is rooted in pins compiled into the signed host app.
use super::{APPLICATIONS_PROTOCOL, WORKER_PROTOCOL, failure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io,
    path::{Path, PathBuf},
};

/// Signed capture service package identity.
pub const PACKAGE: &str = "so.tilo.imager.home-export";
#[cfg(not(feature = "capture-worker"))]
const EMBEDDED: &str = include_str!(concat!(env!("OUT_DIR"), "/capture-service.json"));
#[cfg(feature = "capture-worker")]
const EMBEDDED: &str = "null";

/// Untrusted pin envelope; only exact signed-artifact verification admits a service.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CaptureServicePins {
    pub schema: u32,
    pub worker_protocol: u32,
    pub arch: String,
    pub package_id: String,
    pub service_publisher: String,
    pub package_hash: String,
    pub manifest_sha256: String,
    pub payload_root_hash: String,
    pub runtime_version: String,
    pub runtime_publisher: String,
    pub runtime_root_hash: String,
}
impl CaptureServicePins {
    pub fn validate(&self) -> io::Result<()> {
        if !matches!(
            self.worker_protocol,
            WORKER_PROTOCOL | APPLICATIONS_PROTOCOL
        ) {
            return Err(failure("Packaged capture worker protocol is incompatible"));
        }
        let digest = |value: &str| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        };
        if self.schema != 1
            || self.arch != std::env::consts::ARCH
            || self.package_id != PACKAGE
            || ![
                &self.service_publisher,
                &self.package_hash,
                &self.manifest_sha256,
                &self.payload_root_hash,
                &self.runtime_publisher,
                &self.runtime_root_hash,
            ]
            .into_iter()
            .all(|value| digest(value))
            || self.runtime_version.is_empty()
            || self.runtime_version.len() > 256
            || !self
                .runtime_version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
        {
            return Err(failure("Packaged capture identity is invalid"));
        }
        Ok(())
    }
}

/// No authority comes from a sidecar publisher field or an environment variable.
pub struct TrustedCaptureService {
    artifacts: CaptureServiceArtifacts,
}

/// Measured artifact facts, not a grant to read or project any source.
/// Constructing these facts cannot construct a trusted service.
pub struct CaptureServiceArtifacts {
    pub service_publisher: String,
    pub root: PathBuf,
    pub publisher: String,
    pub manifest: PathBuf,
    pub payload: PathBuf,
    pub package_hash: String,
    pub installed: package_store::InstalledPackage,
}
impl std::ops::Deref for TrustedCaptureService {
    type Target = CaptureServiceArtifacts;
    fn deref(&self) -> &Self::Target {
        &self.artifacts
    }
}

impl TrustedCaptureService {
    pub fn native() -> io::Result<Self> {
        installed_original_consumer_facts().map(|(service, _, _)| service)
    }

    pub fn native_for_protocol(required: u32) -> io::Result<Self> {
        let (service, pins, _) = installed_original_consumer_facts()?;
        if !matches!(required, WORKER_PROTOCOL | APPLICATIONS_PROTOCOL)
            || pins.worker_protocol < required
        {
            return Err(failure(
                "Selected original phase requires a newer signed worker",
            ));
        }
        Ok(service)
    }
    fn verify(sidecar: &Path, pins: &CaptureServicePins) -> io::Result<Self> {
        pins.validate()?;
        let metadata = sidecar.join("service/metadata");
        for name in ["signed-root", "manifest.toml", "collection.qcl"] {
            let path = metadata.join(name);
            let facts = std::fs::symlink_metadata(&path)?;
            if !facts.is_file() || facts.len() > 8 * 1024 * 1024 {
                return Err(failure(
                    "Packaged capture metadata is not a bounded regular file",
                ));
            }
        }
        let record = package_store::integrity::read_record(&metadata, PACKAGE, &pins.package_hash)
            .map_err(io::Error::other)?;
        if hex(&record.root.publisher_identity) != pins.service_publisher
            || record.root.payload_root_hash.to_hex() != pins.payload_root_hash
            || hex(&Sha256::digest(&record.manifest_bytes)) != pins.manifest_sha256
        {
            return Err(failure(
                "Packaged capture service differs from its embedded identity",
            ));
        }
        let manifest = tilo_app_package::Manifest::from_toml_str(
            std::str::from_utf8(&record.manifest_bytes).map_err(io::Error::other)?,
        )
        .map_err(io::Error::other)?;
        if manifest
            .package
            .service_protocol
            .as_ref()
            .is_none_or(|protocol| {
                protocol.name != "tilo.home-capture" || protocol.version != pins.worker_protocol
            })
        {
            return Err(failure(
                "Packaged capture service worker protocol differs from its embedded identity",
            ));
        }
        if manifest.package.id != PACKAGE
            || manifest.package.runtime.as_deref()
                != Some(format!("tilo-runtime/{}", pins.runtime_version).as_str())
        {
            return Err(failure(
                "Packaged capture service declares a different package or runtime",
            ));
        }
        let payload = sidecar.join("service/payload");
        let manifest_path = metadata.join("manifest.toml");
        let mut installed = launcher::managed::agent_package_row(
            pins.package_hash.clone(),
            payload.clone(),
            manifest_path.clone(),
        );
        installed.id = PACKAGE.into();
        installed.version = manifest.package.version;
        installed
            .publisher_key_hex
            .clone_from(&pins.service_publisher);
        installed.runtime = format!("tilo-runtime/{}", pins.runtime_version);
        let report =
            package_store::integrity::verify(&installed, &payload).map_err(io::Error::other)?;
        if !report.is_intact() || !report.unexpected.is_empty() {
            return Err(failure(
                "Packaged capture payload differs from its signed collection",
            ));
        }
        let root = sidecar.join("runtime").join(&pins.runtime_version);
        let runtime = runtime_bundle::verify_bundle_with(
            &root,
            &runtime_bundle::VerifyOptions {
                require_publisher: Some(pins.runtime_publisher.clone()),
                require_signature: true,
                require_arch: Some(pins.arch.clone()),
            },
        )
        .map_err(io::Error::other)?;
        if runtime.version != pins.runtime_version || runtime.root_hash != pins.runtime_root_hash {
            return Err(failure(
                "Packaged capture runtime differs from its embedded identity",
            ));
        }
        Ok(Self {
            artifacts: CaptureServiceArtifacts {
                service_publisher: pins.service_publisher.clone(),
                root,
                publisher: pins.runtime_publisher.clone(),
                manifest: manifest_path,
                payload,
                package_hash: pins.package_hash.clone(),
                installed,
            },
        })
    }
}
/// Returns only artifacts verified against the pins compiled into this helper.
/// Receipt metadata never selects a service, publisher, runtime, or sidecar.
pub fn installed_original_consumer_facts()
-> io::Result<(TrustedCaptureService, CaptureServicePins, [u8; 32])> {
    let pins: Option<CaptureServicePins> = serde_json::from_str(EMBEDDED)?;
    let pins = pins.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "This package has no qualified migration service",
        )
    })?;
    let executable = std::env::current_exe()?;
    let service = TrustedCaptureService::verify(&sidecar_for_executable(&executable)?, &pins)?;
    Ok((service, pins, Sha256::digest(EMBEDDED.as_bytes()).into()))
}

/// Verify candidate artifact facts without granting native service admission.
/// The signed publisher, payload, protocol and runtime must match every pin.
/// Native source custody still requires the separately authenticated caller.
pub fn verify_sidecar(
    sidecar: &Path,
    pins: &CaptureServicePins,
) -> io::Result<CaptureServiceArtifacts> {
    TrustedCaptureService::verify(sidecar, pins).map(|service| service.artifacts)
}

fn sidecar_for_executable(executable: &Path) -> io::Result<PathBuf> {
    if executable == Path::new("/usr/libexec/tilo-prepared-setup") {
        return Ok(PathBuf::from("/usr/share/tilo-imager/capture"));
    }
    let directory = executable
        .parent()
        .ok_or_else(|| failure("Host package location is missing"))?;
    // Existing native host/AppImage/Flatpak bin prefix and portable tar prefix.
    let prefix = if directory.file_name().is_some_and(|name| name == "bin") {
        directory
            .parent()
            .ok_or_else(|| failure("Host package prefix is missing"))?
    } else {
        directory
    };
    Ok(prefix.join("share/tilo-imager/capture"))
}
fn hex(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn installed_controller_uses_only_its_exact_fixed_capture_sidecar() {
        assert_eq!(
            sidecar_for_executable(Path::new("/usr/libexec/tilo-prepared-setup")).unwrap(),
            Path::new("/usr/share/tilo-imager/capture")
        );
        for executable in [
            "/tmp/tilo-prepared-setup",
            "/usr/libexec/tilo-prepared-setup-other",
            "/usr/libexec/other",
        ] {
            assert_ne!(
                sidecar_for_executable(Path::new(executable)).unwrap(),
                Path::new("/usr/share/tilo-imager/capture")
            );
        }
        assert_eq!(
            sidecar_for_executable(Path::new("/portable/bin/tilo-imager")).unwrap(),
            Path::new("/portable/share/tilo-imager/capture")
        );
        assert_eq!(
            sidecar_for_executable(Path::new("/portable/tilo-imager")).unwrap(),
            Path::new("/portable/share/tilo-imager/capture")
        );
    }
    #[test]
    fn signed_sidecar_requires_embedded_identity_and_exact_payload() {
        let directory = tempfile::tempdir().unwrap();
        let sidecar = directory.path();
        let native = sidecar.join("host");
        let loader = if std::env::consts::ARCH == "aarch64" {
            "lib/ld-linux-aarch64.so.1"
        } else {
            "lib64/ld-linux-x86-64.so.2"
        };
        std::fs::create_dir_all(native.join(loader).parent().unwrap()).unwrap();
        std::fs::write(native.join(loader), b"fixture native loader").unwrap();
        let identity = runtime_bundle::Identity::generate();
        let identity_path = sidecar.join("runtime-identity.json");
        std::fs::write(&identity_path, identity.to_json_bytes()).unwrap();
        let (runtime, _) = runtime_bundle::build_abi_bundle(
            &sidecar.join("runtime"),
            "2026.07",
            &runtime_bundle::BuildOptions {
                from: Some(&native),
                identity: Some(&identity_path),
                arch: Some(std::env::consts::ARCH),
                ..Default::default()
            },
        )
        .unwrap();
        let payload = sidecar.join("source");
        std::fs::create_dir_all(payload.join("bin")).unwrap();
        std::fs::write(
            payload.join("bin/tilo-home-export"),
            b"signed worker fixture",
        )
        .unwrap();
        let signer = tilo_app_package::SigningIdentity::generate();
        let publisher = hex(&signer.public_key_bytes());
        let manifest = tilo_app_package::Manifest::from_toml_str(&format!(
            r#"
[package]
id = "{PACKAGE}"
version = "1.0.0"
kind = "service"
runtime = "tilo-runtime/{}"
[package.service-protocol]
name = "tilo.home-capture"
version = {WORKER_PROTOCOL}
[publisher]
identity = "ed25519:{publisher}"
signature = ""
[sandbox]
namespaces = ["user", "pid", "uts", "ipc", "mount"]
seccomp = "tilo-default"
landlock = "strict"
[sandbox.net]
mode = "off"
[entrypoint]
bin = "bin/tilo-home-export"
type = "cli"
"#,
            runtime.version
        ))
        .unwrap();
        let packed = tilo_app_package::pack(&payload, &manifest, &signer).unwrap();
        let metadata = sidecar.join("service/metadata");
        std::fs::create_dir_all(&metadata).unwrap();
        for (name, bytes) in [
            ("signed-root", packed.meta.signed_root_bytes().to_vec()),
            ("manifest.toml", packed.meta.manifest_bytes.clone()),
            ("collection.qcl", packed.meta.collection_bytes.clone()),
        ] {
            std::fs::write(metadata.join(name), bytes).unwrap();
        }
        tilo_app_package::unpack(&packed.meta, &packed, &sidecar.join("service/payload")).unwrap();
        let pins = CaptureServicePins {
            schema: 1,
            worker_protocol: WORKER_PROTOCOL,
            arch: std::env::consts::ARCH.into(),
            package_id: PACKAGE.into(),
            service_publisher: publisher,
            package_hash: packed.id().to_hex(),
            manifest_sha256: hex(&Sha256::digest(&packed.meta.manifest_bytes)),
            payload_root_hash: packed.meta.root.payload_root_hash.to_hex(),
            runtime_version: runtime.version,
            runtime_publisher: identity.public_key_hex(),
            runtime_root_hash: runtime.root_hash,
        };
        assert!(TrustedCaptureService::verify(sidecar, &pins).is_ok());
        assert_eq!(
            manifest.package.service_protocol.as_ref().unwrap().version,
            pins.worker_protocol
        );
        let mut wrong = pins.clone();
        wrong.worker_protocol = 1;
        let error = TrustedCaptureService::verify(&sidecar.join("not-created"), &wrong)
            .err()
            .unwrap();
        assert!(error.to_string().contains("worker protocol"));
        wrong = pins.clone();
        wrong.service_publisher = "00".repeat(32);
        assert!(TrustedCaptureService::verify(sidecar, &wrong).is_err());
        wrong = pins.clone();
        wrong.runtime_root_hash = "00".repeat(32);
        assert!(TrustedCaptureService::verify(sidecar, &wrong).is_err());
        std::fs::write(sidecar.join("service/payload/extra"), b"unsigned extra").unwrap();
        assert!(TrustedCaptureService::verify(sidecar, &pins).is_err());
        std::fs::remove_file(sidecar.join("service/payload/extra")).unwrap();
        std::fs::write(
            sidecar.join("service/payload/bin/tilo-home-export"),
            b"substituted worker",
        )
        .unwrap();
        assert!(TrustedCaptureService::verify(sidecar, &pins).is_err());
    }
    #[test]
    fn missing_protocol_pin_is_not_accepted_as_legacy_capability() {
        assert!(serde_json::from_str::<CaptureServicePins>(r#"{"schema":1,"arch":"x86_64","package_id":"so.tilo.imager.home-export","service_publisher":"00","package_hash":"00","manifest_sha256":"00","payload_root_hash":"00","runtime_version":"1","runtime_publisher":"00","runtime_root_hash":"00"}"#).is_err());
    }
}
