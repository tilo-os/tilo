//! `--builder remote`: the component builds on a remote builder of the
//! machine's architecture and its layer streams from there through this
//! host into the device as content ([`crate::builderlayer`]); no
//! binary is written to this host's disk.

use std::io::BufReader;
use std::process::{Command, Stdio};

use tilo_device_protocol::LayerSource;
use tilo_device_protocol::manifest::Component;
use tilo_device_protocol::{AppInstall, AppInstallResult};
use tilo_device_protocol::{LayerImport, LayerList, LayerVerify};

use super::{App, Resolved};
use crate::build;
use crate::builderlayer::{self, Stream};
use crate::error::CliError;
use crate::hypervisor;
use crate::remote::Remote;

fn build_header<T>(
    remote: &Remote,
    device: bool,
    build: impl FnOnce() -> std::io::Result<T>,
) -> Result<T, CliError> {
    let (result, renewal) = if device {
        remote.keep_awake_while(build)?
    } else {
        (build(), Ok(()))
    };
    let header = result.map_err(CliError::io("read the remote builder's header"))?;
    renewal?;
    Ok(header)
}

fn check_import_hash(packed: &builderlayer::Packed, imported: &str) -> Result<(), CliError> {
    if packed
        .layer_hash
        .as_deref()
        .is_some_and(|hash| hash != imported)
    {
        return Err(CliError::InvalidInput(
            "imported layer content differs from the remote assembler".into(),
        ));
    }
    Ok(())
}

fn start_builder(
    line: &hypervisor::CommandLine,
    root: &std::path::Path,
) -> Result<std::process::Child, CliError> {
    Command::new(&line.program)
        .args(&line.args)
        .envs(line.env.iter().map(|(k, v)| (k, v)))
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                CliError::ToolMissing {
                    tool: line.program.clone(),
                    purpose: "build on the remote builder (--builder remote)".into(),
                }
            } else {
                CliError::io("start the remote builder")(e)
            }
        })
}

#[derive(Debug, PartialEq, Eq)]
enum Delivery {
    Reused(String),
    Uploaded(String),
    Invalidated,
}

fn deliver_layer(
    remote: &mut Remote,
    stream: &mut impl std::io::Read,
    packed: &builderlayer::Packed,
    component: &str,
    claim: &tilo_device_protocol::Provenance,
) -> Result<Delivery, CliError> {
    if !packed.reused {
        return remote.put_reader(stream).map(Delivery::Uploaded);
    }
    if stream
        .read(&mut [0u8; 1])
        .map_err(CliError::io("verify reused layer stream"))?
        != 0
    {
        return Err(CliError::InvalidInput(
            "reused layer stream contains archive bytes".into(),
        ));
    }
    let hash = packed.layer_hash.as_deref().unwrap_or_default();
    if verified_layer(remote, hash, component, Some(claim)) {
        Ok(Delivery::Reused(hash.to_owned()))
    } else {
        Ok(Delivery::Invalidated)
    }
}

fn verified_layer(
    remote: &mut Remote,
    hash: &str,
    component: &str,
    claim: Option<&tilo_device_protocol::Provenance>,
) -> bool {
    remote
        .call(LayerVerify {
            hash: hash.to_owned(),
        })
        .is_ok_and(|layer| {
            layer.hash == hash
                && layer.provenance.component == component
                && layer.provenance.target.as_deref()
                    == Some(format!("{}-unknown-linux-gnu", remote.status.arch).as_str())
                && claim.is_none_or(|claim| layer.provenance.interfaces == claim.interfaces)
        })
}

fn verified_layers(remote: &mut Remote, component: &str) -> Result<Vec<String>, CliError> {
    let listed = remote.call(LayerList {})?;
    Ok(listed
        .into_iter()
        .filter(|layer| layer.provenance.component == component)
        .rev()
        .take(8)
        .filter(|layer| verified_layer(remote, &layer.hash, component, None))
        .map(|layer| layer.hash)
        .collect())
}

pub(super) fn validate_header(
    packed: &builderlayer::Packed,
    component: &str,
    digest: &str,
    arch: &str,
    cached: &[String],
) -> Result<(), CliError> {
    if packed.component != component
        || packed.source_digest.as_deref() != Some(digest)
        || packed.target.as_deref() != Some(format!("{arch}-unknown-linux-gnu").as_str())
        || (packed.reused
            && !packed
                .layer_hash
                .as_ref()
                .is_some_and(|hash| cached.contains(hash)))
    {
        return Err(CliError::InvalidInput("remote component source, architecture or cache result does not match the requested build".into()));
    }
    Ok(())
}

fn builder_header_error(
    child: &mut AppBuilder,
    line: &hypervisor::CommandLine,
    error: &CliError,
) -> CliError {
    let _ = child.0.kill();
    let status = child
        .0
        .wait()
        .map_or_else(|e| e.to_string(), |s| s.to_string());
    CliError::CommandFailed {
        command: line.display(),
        status,
        stderr: format!("{error}; the build's output is above"),
    }
}

fn builder_succeeded(
    child: &mut AppBuilder,
    line: &hypervisor::CommandLine,
) -> Result<(), CliError> {
    let status = child
        .0
        .wait()
        .map_err(CliError::io("wait for the remote builder"))?;
    if !status.success() {
        return Err(CliError::CommandFailed {
            command: line.display(),
            status: status.to_string(),
            stderr: "the remote builder failed after sending the layer; its output is above".into(),
        });
    }
    Ok(())
}

impl App {
    pub(super) fn build_layer_remote(
        &mut self,
        remote: &mut Remote,
        vm: &Resolved,
        component: &Component,
        builder: &[String],
    ) -> Result<String, CliError> {
        let worktree = self.worktree.clone().ok_or(CliError::NotInWorktree)?;
        self.runner.cwd = Some(worktree.root.clone());
        let metadata = self.cargo_metadata()?;
        let extra = build::dev_features(&metadata, &component.packages);
        let inputs = crate::source_inputs::Snapshot::create(&worktree, &component.name)?;
        self.build_layer_remote_cached(remote, vm, component, builder, &inputs, &extra, true)
            .map(|member| member.layer)
    }

    pub(super) fn build_layer_remote_inventory(
        &mut self,
        remote: &mut Remote,
        vm: &Resolved,
        component: &Component,
        builder: &[String],
        inputs: &crate::source_inputs::Snapshot,
    ) -> Result<tilo_device_protocol::UserspaceMember, CliError> {
        let worktree = self.worktree.clone().ok_or(CliError::NotInWorktree)?;
        self.runner.cwd = Some(worktree.root);
        let metadata = self.cargo_metadata()?;
        let extra = build::dev_features(&metadata, &component.packages);
        self.build_layer_remote_cached(remote, vm, component, builder, inputs, &extra, true)
    }

    pub(super) fn build_layer_local_pack(
        &mut self,
        remote: &mut Remote,
        vm: &Resolved,
        component: &Component,
    ) -> Result<String, CliError> {
        let worktree = self.worktree.clone().ok_or(CliError::NotInWorktree)?;
        self.runner.cwd = Some(worktree.root.clone());
        let metadata = self.cargo_metadata()?;
        let extra = build::dev_features(&metadata, &component.packages);
        let inputs = crate::source_inputs::Snapshot::create(&worktree, &component.name)?;
        self.build_layer_remote_cached(remote, vm, component, &[], &inputs, &extra, true)
            .map(|member| member.layer)
    }

    fn build_layer_remote_cached(
        &mut self,
        remote: &mut Remote,
        vm: &Resolved,
        component: &Component,
        builder: &[String],
        inputs: &crate::source_inputs::Snapshot,
        extra: &[String],
        reuse: bool,
    ) -> Result<tilo_device_protocol::UserspaceMember, CliError> {
        let worktree = self.worktree.clone().ok_or(CliError::NotInWorktree)?;
        let mut provenance = inputs.provenance.clone();
        provenance.component.clone_from(&component.name);
        inputs.unchanged(&worktree.root)?;
        let label = if builder.is_empty() { "host" } else { "remote" };
        provenance.builder = Some(label.to_owned());
        let inventory = inputs
            .file
            .path()
            .strip_prefix(&worktree.root)
            .map_err(|error| CliError::InvalidInput(error.to_string()))?;
        let cached = if reuse {
            self.receipt
                .phase("layer_lookup", || verified_layers(remote, &component.name))?
        } else {
            Vec::new()
        };
        let mut line = builderlayer::remote_command(builder, component, extra)
            .arg("--source-inventory")
            .arg(inventory.display().to_string());
        for hash in &cached {
            line = line.arg("--cached-layer").arg(hash);
        }
        if !self.json {
            eprintln!(
                "tilo-vm: building {} with {} ({}); the layer streams to {} without touching this disk",
                component.name,
                if label == "host" {
                    "this host's cargo"
                } else {
                    "the remote builder"
                },
                line.display(),
                vm.name
            );
        }
        let mut child = AppBuilder(start_builder(&line, &worktree.root)?);
        let stdout = child.0.stdout.take().ok_or_else(|| {
            CliError::io("start the remote builder")(std::io::Error::other("no stdout"))
        })?;
        let mut stream = Stream::new(BufReader::with_capacity(2 << 20, stdout));
        let header = self.receipt.phase("build", || {
            build_header(remote, vm.entry.hypervisor == hypervisor::DEVICE, || {
                stream.header(&mut std::io::stderr())
            })
        });
        let packed = match header {
            Ok(packed) => packed,
            Err(error) => {
                drop(stream);
                return Err(builder_header_error(&mut child, &line, &error));
            }
        };
        validate_header(
            &packed,
            &component.name,
            &inputs.digest,
            &remote.status.arch,
            &cached,
        )?;
        *remote = self.connect_with_wake(vm, false)?;
        let delivered = self.receipt.phase("transfer", || {
            deliver_layer(remote, &mut stream, &packed, &component.name, &provenance)
        });
        drop(stream);
        let delivered = delivered?;
        builder_succeeded(&mut child, &line)?;
        inputs.unchanged(&worktree.root)?;
        provenance.cargo_profile = Some(component.profile.clone());
        provenance.features = component.build_features(extra);
        provenance.target.clone_from(&packed.target);
        provenance.toolchain.clone_from(&packed.toolchain);
        let upload = match delivered {
            Delivery::Reused(hash) => {
                self.receipt.set("upload", "skipped");
                self.receipt.set("layer_reused", &hash);
                return Ok(userspace_member(component, hash, provenance, inputs));
            }
            Delivery::Invalidated => {
                self.receipt.set("remote_cache", "invalidated");
                return self.build_layer_remote_cached(
                    remote, vm, component, builder, inputs, extra, false,
                );
            }
            Delivery::Uploaded(upload) => upload,
        };
        let imported = self.receipt.phase("import", || {
            remote.call(LayerImport {
                source: LayerSource::Content { hash: upload },
                provenance: Box::new(provenance.clone()),
            })
        })?;
        check_import_hash(&packed, &imported.hash)?;
        Ok(userspace_member(
            component,
            imported.hash,
            provenance,
            inputs,
        ))
    }

    pub(super) fn install_app_from_remote(
        &mut self,
        remote: &mut Remote,
        vm: &Resolved,
        account: &str,
        dir: &str,
        builder: &[String],
    ) -> Result<AppInstallResult, CliError> {
        let worktree = self.worktree.clone().ok_or(CliError::NotInWorktree)?;
        let provenance = build::source_provenance(&worktree, &format!("app:{dir}"), "remote");
        let inputs = crate::source_inputs::Snapshot::create(&worktree, &format!("app:{dir}"))?;
        let inventory = inputs
            .file
            .path()
            .strip_prefix(&worktree.root)
            .map_err(|e| CliError::InvalidInput(e.to_string()))?;
        let line = crate::builderapp::remote_command(builder, dir, inventory)?;
        let arch = remote.status.arch.clone();
        self.receipt.set("builder", "remote");
        self.receipt.set("app_source", provenance);
        let mut child = AppBuilder(start_builder(&line, &worktree.root)?);
        let stdout = child
            .0
            .stdout
            .take()
            .ok_or_else(|| CliError::InvalidInput("remote app builder has no stdout".into()))?;
        let mut stream = Stream::new(BufReader::with_capacity(2 << 20, stdout));
        let packed: crate::builderapp::PackedApp = self.receipt.phase("build", || {
            build_header(remote, vm.entry.hypervisor == hypervisor::DEVICE, || {
                stream.header_as(&mut std::io::stderr())
            })
        })?;
        packed.validate(dir, &arch, &inputs.digest)?;
        self.receipt.set("app_package", &packed);
        *remote = self.connect(vm)?;
        let upload = self
            .receipt
            .phase("transfer", || remote.put_reader(&mut stream));
        // Drop the pipe before waiting: a refused upload must not deadlock a
        // builder still blocked writing its remaining package bytes.
        drop(stream);
        let upload = upload?;
        let status = child
            .0
            .wait()
            .map_err(CliError::io("wait for remote app builder"))?;
        if !status.success() {
            return Err(CliError::CommandFailed {
                command: line.display(),
                status: status.to_string(),
                stderr: "remote app builder failed; no app was installed".into(),
            });
        }
        self.receipt.phase("install", || {
            remote.call(AppInstall {
                account: account.into(),
                source: LayerSource::Content { hash: upload },
            })
        })
    }

    pub(super) fn no_remote_builder(&self, arch: &str) -> CliError {
        CliError::Config {
            path: self.registry_path.display().to_string(),
            detail: format!(
                "no remote builder for {arch} is configured; add `[remote_builders]` with `{arch} = [\"<a cargo drop-in whose run runs on an {arch} machine>\", ...]` (tilo-vm guide swapping)"
            ),
        }
    }
}

fn userspace_member(
    component: &Component,
    layer: String,
    build: tilo_device_protocol::Provenance,
    inputs: &crate::source_inputs::Snapshot,
) -> tilo_device_protocol::UserspaceMember {
    tilo_device_protocol::UserspaceMember {
        component: component.name.clone(),
        layer,
        build,
        source_inventory: inputs.digest.clone(),
    }
}

/// Every failure path terminates and reaps only its owned builder process.
struct AppBuilder(std::process::Child);

impl Drop for AppBuilder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use serde_json::json;
    use tilo_device_protocol::{RpcError, code};
    use tilo_test_env::SpawnUnderEnvLock as _;

    use super::*;
    use crate::remote::fixture::Device;

    fn claim() -> tilo_device_protocol::Provenance {
        tilo_device_protocol::Provenance::local("shell")
    }

    /// An `x86_64` device that lists `hash` as a `shell` layer and verifies it as `mode` says:
    /// `valid`, gone (`missing`, `corrupt`, `old`), or recorded as another component,
    /// architecture or interface declaration.
    fn cache_peer(hash: &str, mode: &'static str) -> Device {
        let hash = hash.to_owned();
        Device::of_arch("x86_64", move |action, _| match action {
            "layer-list" => Ok(json!([{ "hash": hash, "provenance": { "component": "shell" } }])),
            "layer-verify" if matches!(mode, "missing" | "corrupt" | "old") => Err(RpcError {
                code: code::NOT_FOUND,
                message: mode.into(),
                hint: String::new(),
                see: None,
            }),
            "layer-verify" => Ok(json!({ "hash": hash, "provenance": {
                "component": if mode == "wrong-component" { "other" } else { "shell" },
                "target": if mode == "wrong-arch" { "aarch64-unknown-linux-gnu" } else { "x86_64-unknown-linux-gnu" },
                "interfaces": if mode == "stale-declaration" { json!({ "runtime": 17 }) } else { json!({}) },
            } })),
            other => panic!("a cached build asks for no {other}"),
        })
    }

    fn connect(device: &Device) -> Remote {
        Remote::connect("test", &device.machine, &device.route).unwrap()
    }

    fn actions(device: &Device) -> Vec<String> {
        device
            .asked()
            .into_iter()
            .map(|asked| asked.action)
            .collect()
    }

    fn framed_fixture(
        tree: &std::path::Path,
        cached: &[String],
    ) -> Result<(builderlayer::Packed, Vec<u8>), CliError> {
        let packed = builderlayer::Packed {
            component: "shell".into(),
            target: Some("x86_64-unknown-linux-gnu".into()),
            source_digest: Some("source-proof".into()),
            ..builderlayer::Packed::default()
        };
        let mut framed = Vec::new();
        builderlayer::write_layer(tree, packed, cached, &mut framed)?;
        let mut stream = Stream::new(framed.as_slice());
        let packed = stream
            .header(&mut Vec::new())
            .map_err(CliError::io("fixture layer header"))?;
        Ok((packed, framed))
    }

    tilo_test_env::scenario! {
        fn the_remote_layer_cache_is_used_only_for_verified_identical_content;

        phase remote_cache_exact_content_skips_archive_and_upload;
        {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join("usr/bin")).unwrap();
            std::fs::write(dir.path().join("usr/bin/shell"), b"fixture payload").unwrap();
            let hash = tilo_device_protocol::layer_hash::tree_hash(dir.path()).unwrap();
            let (packed, framed) = framed_fixture(dir.path(), std::slice::from_ref(&hash)).unwrap();
            assert!(packed.reused);
            assert!(!String::from_utf8_lossy(&framed).contains(builderlayer::DATA));
            let device = cache_peer(&hash, "valid");
            let mut remote = connect(&device);
            assert_eq!(
                verified_layers(&mut remote, "shell").unwrap(),
                vec![hash.clone()]
            );
            validate_header(
                &packed,
                "shell",
                "source-proof",
                "x86_64",
                std::slice::from_ref(&hash),
            )
            .unwrap();
            let mut stream = Stream::new(framed.as_slice());
            stream.header(&mut Vec::new()).unwrap();
            assert_eq!(
                deliver_layer(&mut remote, &mut stream, &packed, "shell", &claim()).unwrap(),
                Delivery::Reused(hash)
            );
            assert_eq!(actions(&device), ["layer-list", "layer-verify", "layer-verify"]);
            assert_eq!(device.content.len(), 0, "nothing was sent");
        }

        phase remote_cache_changed_content_streams_normally;
        {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join("usr/bin")).unwrap();
            let file = dir.path().join("usr/bin/shell");
            std::fs::write(&file, b"old payload").unwrap();
            let old = tilo_device_protocol::layer_hash::tree_hash(dir.path()).unwrap();
            std::fs::write(&file, b"new payload").unwrap();
            let (packed, framed) = framed_fixture(dir.path(), std::slice::from_ref(&old)).unwrap();
            assert!(!packed.reused);
            assert_ne!(packed.layer_hash.as_deref(), Some(old.as_str()));
            let device = cache_peer(&old, "valid");
            let mut remote = connect(&device);
            let mut stream = Stream::new(framed.as_slice());
            stream.header(&mut Vec::new()).unwrap();
            let Delivery::Uploaded(put) =
                deliver_layer(&mut remote, &mut stream, &packed, "shell", &claim()).unwrap()
            else {
                panic!("changed content must be sent");
            };
            assert!(device.content.get(&put).is_some_and(|tar| !tar.is_empty()));
            assert!(actions(&device).is_empty());
        }

        phase an_identical_layer_with_an_older_build_claim_is_sent_again;
        {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join("usr/bin")).unwrap();
            std::fs::write(dir.path().join("usr/bin/shell"), b"fixture payload").unwrap();
            let hash = tilo_device_protocol::layer_hash::tree_hash(dir.path()).unwrap();
            let (packed, framed) = framed_fixture(dir.path(), std::slice::from_ref(&hash)).unwrap();
            let device = cache_peer(&hash, "stale-declaration");
            let mut remote = connect(&device);
            let mut stream = Stream::new(framed.as_slice());
            stream.header(&mut Vec::new()).unwrap();
            let mut claim = claim();
            claim.interfaces.insert("runtime".into(), 18);
            assert_eq!(
                deliver_layer(&mut remote, &mut stream, &packed, "shell", &claim).unwrap(),
                Delivery::Invalidated,
                "the daemon records another declaration for these bytes, so they travel again with this tree's claim"
            );
        }

        phase remote_cache_invalid_peer_falls_back_and_stale_offer_is_invalidated;
        {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join("usr/bin")).unwrap();
            std::fs::write(dir.path().join("usr/bin/shell"), b"fixture payload").unwrap();
            let hash = tilo_device_protocol::layer_hash::tree_hash(dir.path()).unwrap();
            let (packed, framed) = framed_fixture(dir.path(), std::slice::from_ref(&hash)).unwrap();
            for mode in ["missing", "corrupt", "old", "wrong-component", "wrong-arch"] {
                let device = cache_peer(&hash, mode);
                let mut remote = connect(&device);
                assert!(verified_layers(&mut remote, "shell").unwrap().is_empty());
                let mut stream = Stream::new(framed.as_slice());
                stream.header(&mut Vec::new()).unwrap();
                assert_eq!(
                    deliver_layer(&mut remote, &mut stream, &packed, "shell", &claim()).unwrap(),
                    Delivery::Invalidated
                );
                assert_eq!(device.content.len(), 0, "{mode}: nothing was sent");
            }
        }

        phase remote_cache_refuses_foreign_source_architecture_and_unoffered_hash;
        {
            let packed = builderlayer::Packed {
                component: "shell".into(),
                target: Some("x86_64-unknown-linux-gnu".into()),
                source_digest: Some("source-proof".into()),
                layer_hash: Some("a".repeat(64)),
                reused: true,
                ..builderlayer::Packed::default()
            };
            let offered = vec!["a".repeat(64)];
            assert!(validate_header(&packed, "shell", "other-source", "x86_64", &offered).is_err());
            assert!(validate_header(&packed, "shell", "source-proof", "aarch64", &offered).is_err());
            assert!(
                validate_header(
                    &packed,
                    "other-component",
                    "source-proof",
                    "x86_64",
                    &offered
                )
                .is_err()
            );
            assert!(validate_header(&packed, "shell", "source-proof", "x86_64", &[]).is_err());
        }

        phase remote_cache_refuses_missing_footer_and_unexpected_archive_data;
        {
            let device = cache_peer(&"a".repeat(64), "valid");
            let mut remote = connect(&device);
            let packed = builderlayer::Packed {
                reused: true,
                layer_hash: Some("a".repeat(64)),
                ..builderlayer::Packed::default()
            };
            let mut cut = Stream::new(&b""[..]);
            assert!(deliver_layer(&mut remote, &mut cut, &packed, "shell", &claim()).is_err());
            let mut framed = Vec::new();
            let mut lines = builderlayer::Lines::new(&mut framed);
            std::io::Write::write_all(&mut lines, b"unexpected archive").unwrap();
            let (sha, bytes) = lines.finish().unwrap();
            std::io::Write::write_all(
                &mut framed,
                format!("{}{sha} {bytes}\n", builderlayer::END).as_bytes(),
            )
            .unwrap();
            let mut stream = Stream::new(framed.as_slice());
            assert!(deliver_layer(&mut remote, &mut stream, &packed, "shell", &claim()).is_err());
            assert!(actions(&device).is_empty());
            assert_eq!(device.content.len(), 0);
        }
    }

    #[test]
    fn remote_app_owned_builder_is_reaped_when_delivery_fails() {
        let child = Command::new("sleep")
            .arg("600")
            .stdout(Stdio::piped())
            .spawn_locked()
            .unwrap();
        let process = std::path::PathBuf::from(format!("/proc/{}", child.id()));
        assert!(process.try_exists().unwrap());
        let guard = AppBuilder(child);
        drop(guard);
        assert!(
            !process.try_exists().unwrap(),
            "owned builder was not reaped"
        );
    }
}
