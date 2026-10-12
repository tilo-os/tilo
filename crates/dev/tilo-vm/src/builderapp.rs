//! The app form of the existing remote builder's checked upload stream.
//! Self-signed development packages still enter the normal `AppInstall` path.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};
use tilo_app_package::{BlobHash, Kind, Verification};

use crate::build;
use crate::builderlayer::{BEGIN, END, Lines, run_to_stderr};
use crate::error::CliError;
use crate::hypervisor::CommandLine;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct PackedApp {
    pub app: String,
    pub architecture: String,
    pub package_id: String,
    pub runtime: Option<String>,
    pub source_digest: String,
}

pub(crate) fn remote_command(
    builder: &[String],
    dir: &str,
    inventory: &Path,
) -> Result<CommandLine, CliError> {
    let (program, prefix) = builder
        .split_first()
        .filter(|(p, _)| !p.is_empty())
        .ok_or_else(|| CliError::InvalidInput("the remote app builder command is empty".into()))?;
    let mut line = CommandLine::new(program, &[]);
    line.args = prefix.to_vec();
    line.args.extend(
        [
            "run", "--locked", "-p", "tilo-vm", "--bin", "tilo-vm", "--", "app-pack", "--app", dir,
        ]
        .map(str::to_owned),
    );
    line.args.push("--source-inventory".into());
    line.args.push(inventory.display().to_string());
    Ok(line)
}

impl PackedApp {
    pub(crate) fn validate(
        &self,
        dir: &str,
        arch: &str,
        source_digest: &str,
    ) -> Result<(), CliError> {
        if self.app != dir || self.architecture != arch || self.source_digest != source_digest {
            return Err(CliError::InvalidInput(
                "the remote app package does not match the requested source or architecture".into(),
            ));
        }
        Ok(())
    }
}

fn inspect(path: &Path, dir: &str, source_digest: String) -> Result<PackedApp, CliError> {
    let invalid = |error: String| CliError::InvalidInput(format!("remote app package: {error}"));
    if std::fs::metadata(path)
        .map_err(CliError::io("stat app package"))?
        .len()
        > tilo_app_package::MAX_APP_TAPP_BYTES
    {
        return Err(invalid("exceeds the native app size limit".into()));
    }
    let mut blobs = BTreeMap::<BlobHash, Vec<u8>>::new();
    let file = std::fs::File::open(path).map_err(CliError::io("open app package"))?;
    let tapp =
        tilo_app_package::tapp::read(file, &mut blobs).map_err(|e| invalid(e.to_string()))?;
    if !tapp.is_complete() || tapp.meta.manifest.package.kind != Kind::App {
        return Err(invalid("requires a complete application package".into()));
    }
    if !matches!(
        tilo_app_package::verify(&tapp.meta, &[]),
        Verification::UntrustedPublisher { .. }
    ) {
        return Err(invalid(
            "requires a valid self-signed development package".into(),
        ));
    }
    let entry = tapp
        .meta
        .executable_path()
        .ok_or_else(|| invalid("no native entrypoint".into()))?;
    let bytes = tapp
        .meta
        .collection
        .entries
        .get(&entry)
        .and_then(|hash| blobs.get(hash))
        .ok_or_else(|| invalid("entrypoint payload missing".into()))?;
    let target = build::elf_facts(bytes)
        .and_then(|facts| facts.target)
        .ok_or_else(|| invalid("entrypoint is not a supported native ELF".into()))?;
    let architecture = target
        .strip_suffix("-unknown-linux-gnu")
        .ok_or_else(|| invalid("entrypoint is not Linux GNU".into()))?
        .to_owned();
    let isa = &tapp.meta.manifest.package.isa;
    let consistent = match architecture.as_str() {
        "aarch64" => isa.is_empty() || isa.aarch64.is_some(),
        "x86_64" => isa.is_empty() || isa.x86_64.is_some(),
        _ => false,
    };
    if !consistent || tapp.meta.manifest.target.is_some() {
        return Err(invalid(
            "native payload and manifest platform/architecture disagree".into(),
        ));
    }
    Ok(PackedApp {
        app: dir.into(),
        architecture,
        package_id: tapp.meta.id.to_string(),
        runtime: tapp.meta.manifest.package.runtime.clone(),
        source_digest,
    })
}

/// Package on the builder, using the same build and signer as the host path.
///
/// # Errors
/// Unknown applications, failed builds, invalid package identity, or I/O.
pub fn pack(dir: &str, inventory: &Path) -> Result<(), CliError> {
    let root = std::env::current_dir().map_err(CliError::io("find app worktree"))?;
    let source_digest = crate::source_inputs::verify(&root, inventory)?;
    let known = build::apps_in(&root);
    if !known.iter().any(|app| app == dir) {
        return Err(CliError::UnknownApp {
            name: dir.into(),
            known,
        });
    }
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let metadata = Command::new(&cargo)
        .args(["metadata", "--locked", "--no-deps", "--format-version", "1"])
        .output()
        .map_err(CliError::io("read builder cargo metadata"))?;
    if !metadata.status.success() {
        return Err(CliError::InvalidInput(
            "builder cargo metadata failed".into(),
        ));
    }
    let metadata = serde_json::from_slice(&metadata.stdout)
        .map_err(|e| CliError::InvalidInput(e.to_string()))?;
    let target = build::target_dir(&metadata);
    let staging = tempfile::tempdir_in(&target).map_err(CliError::io("stage remote app"))?;
    let tapp = staging.path().join("app.tapp");
    let package = build::app_package(&target, dir, &tapp);
    run_to_stderr(
        Command::new(&package.program)
            .args(&package.args)
            .envs(package.env.iter().map(|(k, v)| (k, v)))
            .env("CARGO", cargo),
    )?;
    let packed = inspect(&tapp, dir, source_digest)?;
    if crate::source_inputs::verify(&root, inventory)? != packed.source_digest {
        return Err(CliError::InvalidInput(
            "source inventory changed while packaging".into(),
        ));
    }
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let header =
        serde_json::to_string(&packed).map_err(|e| CliError::InvalidInput(e.to_string()))?;
    writeln!(out, "{BEGIN}{header}").map_err(CliError::io("write app header"))?;
    let mut lines = Lines::new(&mut out);
    let mut file = std::fs::File::open(tapp).map_err(CliError::io("open packed app"))?;
    std::io::copy(&mut file, &mut lines).map_err(CliError::io("stream packed app"))?;
    let (sha, bytes) = lines.finish().map_err(CliError::io("finish packed app"))?;
    writeln!(out, "{END}{sha} {bytes}").map_err(CliError::io("write app trailer"))?;
    out.flush().map_err(CliError::io("flush packed app"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_app_inspects_signed_payload_and_rejects_wrong_architecture_metadata() {
        use tilo_app_package::{BlobSource, Manifest, SigningIdentity, pack};
        let root = tempfile::tempdir().unwrap();
        let payload = root.path().join("payload");
        std::fs::create_dir_all(payload.join("bin")).unwrap();
        let mut elf = vec![0; 64];
        elf.get_mut(..6)
            .unwrap()
            .copy_from_slice(b"\x7fELF\x02\x01");
        elf.get_mut(0x12..0x14)
            .unwrap()
            .copy_from_slice(&0xb7_u16.to_le_bytes());
        std::fs::write(payload.join("bin/fixture"), elf).unwrap();
        let source = r#"
[package]
id = "so.tilo.fixture"
version = "0.1.0"
kind = "app"
runtime = "tilo-runtime/2026.07"
[package.isa]
aarch64 = "aarch64"
[publisher]
identity = "ed25519:0000000000000000000000000000000000000000000000000000000000000000"
signature = ""
[sandbox]
namespaces = ["user", "pid", "mount"]
seccomp = "tilo-default"
landlock = "strict"
[sandbox.net]
mode = "off"
[sandbox.portals]
[entrypoint]
bin = "bin/fixture"
type = "gui"
"#;
        let identity = SigningIdentity::from_seed(&[23; 32]);
        let make = |text: &str, path: &Path| {
            let mut manifest = Manifest::from_toml_str(text).unwrap();
            manifest.publisher.identity =
                format!("ed25519:{}", hex::encode(identity.public_key_bytes()));
            let package = pack(&payload, &manifest, &identity).unwrap();
            let file = std::fs::File::create(path).unwrap();
            tilo_app_package::tapp::write(file, &package.meta, Some(&package as &dyn BlobSource))
                .unwrap();
        };
        let good = root.path().join("good.tapp");
        make(source, &good);
        let packed = inspect(&good, "fixture", "all-source".into()).unwrap();
        assert_eq!(packed.architecture, "aarch64");
        assert_eq!(packed.runtime.as_deref(), Some("tilo-runtime/2026.07"));
        assert!(packed.validate("fixture", "x86_64", "all-source").is_err());
        let bad = root.path().join("wrong-isa.tapp");
        make(
            &source.replace("aarch64 = \"aarch64\"", "x86_64 = \"v3\""),
            &bad,
        );
        assert!(inspect(&bad, "fixture", "all-source".into()).is_err());
    }

    #[test]
    fn remote_app_command_and_header_never_fall_back_or_accept_other_sources() {
        assert!(remote_command(&[], "terminal", Path::new("inputs.json")).is_err());
        let line = remote_command(
            &["cargo-dropin".into(), "--on-builder".into()],
            "terminal",
            Path::new("inputs.json"),
        )
        .unwrap();
        assert_eq!(line.program, "cargo-dropin");
        assert_eq!(line.args.first().map(String::as_str), Some("--on-builder"));
        assert!(
            line.args
                .windows(2)
                .any(|pair| pair == ["--source-inventory", "inputs.json"])
        );
        let packed = PackedApp {
            app: "terminal".into(),
            architecture: "aarch64".into(),
            package_id: "fixture".into(),
            runtime: None,
            source_digest: "source".into(),
        };
        assert!(packed.validate("terminal", "aarch64", "source").is_ok());
        assert!(packed.validate("terminal", "x86_64", "source").is_err());
        assert!(
            packed
                .validate("terminal", "aarch64", "different SDK")
                .is_err()
        );
        assert!(packed.validate("notes", "aarch64", "source").is_err());
    }
}
