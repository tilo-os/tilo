//! `--builder remote`: the component builds on a remote builder of the
//! machine's architecture and its layer streams from there through this
//! host straight into tilo-vmd's upload ([`crate::builderlayer`]); no
//! binary is written to this host's disk.

use std::io::BufReader;
use std::process::{Command, Stdio};

use tilo_vm_protocol::LayerSource;
use tilo_vm_protocol::manifest::Component;
use tilo_vm_protocol::{AppInstall, AppInstallResult};
use tilo_vm_protocol::{LayerImport, LayerList, LayerVerify};

use super::{App, Resolved};
use crate::build;
use crate::builderlayer::{self, Stream};
use crate::error::CliError;
use crate::hypervisor;
use crate::remote::Remote;

fn build_header<T>(
    remote: &mut Remote,
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
) -> Result<Delivery, CliError> {
    if !packed.reused {
        return remote.upload(stream).map(Delivery::Uploaded);
    }
    // Require the normal checksum footer, with no unexpected archive DATA.
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
    if verified_layer(remote, hash, component) {
        Ok(Delivery::Reused(hash.to_owned()))
    } else {
        // Cache damage or GC after the offer causes one plain-stream retry.
        Ok(Delivery::Invalidated)
    }
}

fn verified_layer(remote: &mut Remote, hash: &str, component: &str) -> bool {
    remote
        .call(LayerVerify {
            hash: hash.to_owned(),
        })
        .is_ok_and(|layer| {
            layer.hash == hash
                && layer.provenance.component == component
                && layer.provenance.target.as_deref()
                    == Some(format!("{}-unknown-linux-gnu", remote.hello.arch).as_str())
        })
}

fn verified_layers(remote: &mut Remote, component: &str) -> Result<Vec<String>, CliError> {
    let listed = remote.call(LayerList {})?;
    Ok(listed
        .into_iter()
        .filter(|layer| layer.provenance.component == component)
        .rev()
        .take(8)
        .filter(|layer| verified_layer(remote, &layer.hash, component))
        .map(|layer| layer.hash)
        .collect())
}

fn validate_header(
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
    /// Build `component` with the remote builder whose command line is
    /// `builder`, and import the layer it streams.
    pub(super) fn build_layer_remote(
        &mut self,
        remote: &mut Remote,
        vm: &Resolved,
        component: &Component,
        builder: &[String],
    ) -> Result<String, CliError> {
        self.build_layer_remote_cached(remote, vm, component, builder, true)
    }

    fn build_layer_remote_cached(
        &mut self,
        remote: &mut Remote,
        vm: &Resolved,
        component: &Component,
        builder: &[String],
        reuse: bool,
    ) -> Result<String, CliError> {
        let worktree = self.worktree.clone().ok_or(CliError::NotInWorktree)?;
        self.runner.cwd = Some(worktree.root.clone());
        let metadata = self.cargo_metadata()?;
        let extra = build::dev_features(&metadata, &component.packages);
        let mut provenance = build::source_provenance(&worktree, &component.name, "remote");
        let inputs = crate::source_inputs::Snapshot::create(&worktree, &component.name)?;
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
        let mut line = builderlayer::remote_command(builder, component, &extra)
            .arg("--source-inventory")
            .arg(inventory.display().to_string());
        for hash in &cached {
            line = line.arg("--cached-layer").arg(hash);
        }
        if !self.json {
            eprintln!(
                "tilo-vm: building {} on the remote builder ({}); the layer streams to {} without touching this disk",
                component.name,
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
                // The owned builder may still be writing its framed layer.
                // Close the reader and reap it before returning an error.
                drop(stream);
                return Err(builder_header_error(&mut child, &line, &error));
            }
        };
        validate_header(
            &packed,
            &component.name,
            &inputs.digest,
            &remote.hello.arch,
            &cached,
        )?;
        // The build can outlast an idle connection: a fresh one for delivery.
        *remote = self.connect(vm)?;
        let delivered = self.receipt.phase("transfer", || {
            deliver_layer(remote, &mut stream, &packed, &component.name)
        });
        drop(stream);
        let delivered = delivered?;
        builder_succeeded(&mut child, &line)?;
        let upload = match delivered {
            Delivery::Reused(hash) => {
                self.receipt.set("upload", "skipped");
                self.receipt.set("layer_reused", &hash);
                return Ok(hash);
            }
            Delivery::Invalidated => {
                self.receipt.set("remote_cache", "invalidated");
                drop(inputs);
                return self.build_layer_remote_cached(remote, vm, component, builder, false);
            }
            Delivery::Uploaded(upload) => upload,
        };
        let expected_hash = packed.layer_hash.clone();
        provenance.cargo_profile = Some(component.profile.clone());
        provenance.features = component.build_features(&extra);
        provenance.target = packed.target;
        provenance.toolchain = packed.toolchain;
        let imported = self.receipt.phase("import", || {
            remote.call(LayerImport {
                source: LayerSource::Upload(upload),
                provenance: Box::new(provenance),
            })
        })?;
        if expected_hash
            .as_deref()
            .is_some_and(|hash| hash != imported.hash)
        {
            return Err(CliError::InvalidInput(
                "imported layer content differs from the remote assembler".into(),
            ));
        }
        Ok(imported.hash)
    }

    /// Build a native app on the configured architecture's builder, then use
    /// the existing slot-user installer; runtime and publisher checks remain there.
    pub(super) fn install_app_from_remote(
        &mut self,
        remote: &mut Remote,
        vm: &Resolved,
        slot: &str,
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
        let arch = remote.hello.arch.clone();
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
            .phase("transfer", || remote.upload(&mut stream));
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
                slot: slot.into(),
                source: LayerSource::Upload(upload),
            })
        })
    }

    /// No remote builder serves `arch`: the line that adds one.
    pub(super) fn no_remote_builder(&self, arch: &str) -> CliError {
        CliError::Config {
            path: self.registry_path.display().to_string(),
            detail: format!(
                "no remote builder for {arch} is configured; add `[remote_builders]` with `{arch} = [\"<a cargo drop-in whose run runs on an {arch} machine>\", ...]` (tilo-vm guide swapping)"
            ),
        }
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    const CACHE_PEER: &str = r#"
import base64,json,sys
hash,mode,log,protocol=sys.argv[1:]
print(json.dumps({"tilo_device_admin":{"device":"test","label":"test","route":"direct","via":"test","session":1}}),flush=True)
received=0
for line in sys.stdin:
 r=json.loads(line); m=r['method']
 with open(log,'a') as f: f.write(m+'\n')
 result={}; error=None
 if m=='hello': result={"protocol":int(protocol),"daemon_version":"test","root":"test","arch":"x86_64"}
 elif m=='layer.list': result=[{"hash":hash,"provenance":{"component":"shell"}}]
 elif m=='layer.verify':
  if mode in ('missing','corrupt','old'): error={"code":-32602,"message":mode}
  else: result={"hash":hash,"provenance":{"component":"other" if mode=='wrong-component' else 'shell',"target":"aarch64-unknown-linux-gnu" if mode=='wrong-arch' else 'x86_64-unknown-linux-gnu'}}
 elif m=='layer.upload.binary': error={"code":-32602,"message":"unknown optional method","hint":"legacy upload"}
 elif m=='layer.upload.begin': result={"upload":"fixture"}
 elif m=='layer.upload.chunk':
  received+=len(base64.b64decode(r['params']['data'])); result={"received":received}
 else: raise RuntimeError(m)
 print(json.dumps({"jsonrpc":"2.0","id":r['id'],"error":error} if error else {"jsonrpc":"2.0","id":r['id'],"result":result}),flush=True)
"#;

    fn cache_peer(hash: &str, mode: &str, log: &std::path::Path) -> Result<Remote, CliError> {
        let line = hypervisor::CommandLine::new("python3", &["-c", CACHE_PEER])
            .arg(hash)
            .arg(mode)
            .arg(log.display().to_string())
            .arg(tilo_vm_protocol::PROTOCOL_VERSION.to_string());
        Remote::over_peer("test", &line).map(|(remote, _)| remote)
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

    #[test]
    fn remote_cache_exact_content_skips_archive_and_upload() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("usr/bin")).unwrap();
        std::fs::write(dir.path().join("usr/bin/shell"), b"fixture payload").unwrap();
        let hash = tilo_vm_protocol::layer_hash::tree_hash(dir.path()).unwrap();
        let (packed, framed) = framed_fixture(dir.path(), std::slice::from_ref(&hash)).unwrap();
        assert!(packed.reused);
        assert!(!String::from_utf8_lossy(&framed).contains(builderlayer::DATA));
        let log = dir.path().join("calls");
        let mut remote = cache_peer(&hash, "valid", &log).unwrap();
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
            deliver_layer(&mut remote, &mut stream, &packed, "shell").unwrap(),
            Delivery::Reused(hash)
        );
        assert_eq!(
            std::fs::read_to_string(log).unwrap(),
            "hello\nlayer.list\nlayer.verify\nlayer.verify\n"
        );
    }

    #[test]
    fn remote_cache_changed_content_streams_normally() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("usr/bin")).unwrap();
        let file = dir.path().join("usr/bin/shell");
        std::fs::write(&file, b"old payload").unwrap();
        let old = tilo_vm_protocol::layer_hash::tree_hash(dir.path()).unwrap();
        std::fs::write(&file, b"new payload").unwrap();
        let (packed, framed) = framed_fixture(dir.path(), std::slice::from_ref(&old)).unwrap();
        assert!(!packed.reused);
        assert_ne!(packed.layer_hash.as_deref(), Some(old.as_str()));
        let log = dir.path().join("calls");
        let mut remote = cache_peer(&old, "valid", &log).unwrap();
        let mut stream = Stream::new(framed.as_slice());
        stream.header(&mut Vec::new()).unwrap();
        assert_eq!(
            deliver_layer(&mut remote, &mut stream, &packed, "shell").unwrap(),
            Delivery::Uploaded("fixture".into())
        );
        assert!(
            std::fs::read_to_string(log)
                .unwrap()
                .contains("layer.upload.chunk")
        );
    }

    #[test]
    fn remote_cache_invalid_peer_falls_back_and_stale_offer_is_invalidated() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("usr/bin")).unwrap();
        std::fs::write(dir.path().join("usr/bin/shell"), b"fixture payload").unwrap();
        let hash = tilo_vm_protocol::layer_hash::tree_hash(dir.path()).unwrap();
        let (packed, framed) = framed_fixture(dir.path(), std::slice::from_ref(&hash)).unwrap();
        for mode in ["missing", "corrupt", "old", "wrong-component", "wrong-arch"] {
            let log = dir.path().join(mode);
            let mut remote = cache_peer(&hash, mode, &log).unwrap();
            assert!(verified_layers(&mut remote, "shell").unwrap().is_empty());
            let mut stream = Stream::new(framed.as_slice());
            stream.header(&mut Vec::new()).unwrap();
            assert_eq!(
                deliver_layer(&mut remote, &mut stream, &packed, "shell").unwrap(),
                Delivery::Invalidated
            );
            assert!(
                !std::fs::read_to_string(log)
                    .unwrap()
                    .contains("layer.upload")
            );
        }
    }

    #[test]
    fn remote_cache_refuses_foreign_source_architecture_and_unoffered_hash() {
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

    #[test]
    fn remote_cache_refuses_missing_footer_and_unexpected_archive_data() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("calls");
        let mut remote = cache_peer(&"a".repeat(64), "valid", &log).unwrap();
        let packed = builderlayer::Packed {
            reused: true,
            layer_hash: Some("a".repeat(64)),
            ..builderlayer::Packed::default()
        };
        let mut cut = Stream::new(&b""[..]);
        assert!(deliver_layer(&mut remote, &mut cut, &packed, "shell").is_err());
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
        assert!(deliver_layer(&mut remote, &mut stream, &packed, "shell").is_err());
        assert_eq!(std::fs::read_to_string(log).unwrap(), "hello\n");
    }

    #[test]
    fn remote_app_owned_builder_is_reaped_when_delivery_fails() {
        let child = Command::new("sleep")
            .arg("600")
            .stdout(Stdio::piped())
            .spawn()
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
