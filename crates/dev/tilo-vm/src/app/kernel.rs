//! `tilo-vm swap kernel` and the kernel half of `tilo-vm rollback --slot vm`.
//!
//! The kernel is the one boot component a dev VM can swap. Its manifest entry
//! names the script that builds it (`dev_build`, os/kernel/dev-build.sh: the
//! release build's source tree, fragments, patches and signing path, kept
//! incremental under `~/.cache/tilo-kernel/`). Then either
//!
//! * install: the staged `usr/lib/modules/<release>/` goes to the VM as a
//!   layer in the `vm` slot (so generations and rollback work as for any
//!   VM-wide component), tilo-vmd merges it, builds the initramfs with the
//!   image's dracut and makes it the next boot entry; tilo-vm reboots the VM
//!   and confirms the new kernel came up, which makes the entry the default;
//! * or `--direct`: the hypervisor boots the host-built vmlinuz and a generic
//!   initramfs itself (libvirt direct kernel boot), for pure boot testing.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};
use tilo_vm_protocol::manifest::Component;
use tilo_vm_protocol::{
    ApplyResult, BootConfirm, BootConfirmResult, LayerImport, LayerSource, Policy, Provenance,
    Rollback, Swap, VM_SLOT,
};

use super::{App, KernelFlags, Outcome, Resolved, describe_apply, human_bytes, on_path, short};
use crate::build;
use crate::config::DirectKernel;
use crate::context::Worktree;
use crate::error::CliError;
use crate::hypervisor::{CommandLine, Runner, SshEndpoint, run_all};
use crate::remote::{self, Remote};
use crate::sshkey;
use crate::vmbuild;

/// How long a rebooted VM gets to answer SSH on its new kernel.
const REBOOT_TIMEOUT: Duration = Duration::from_secs(240);

/// What os/kernel/dev-build.sh writes to `--result`.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct Built {
    pub kver: String,
    /// `uname -v` of the built kernel.
    pub uts_version: String,
    /// The staged `lib/modules/<kver>/` (vmlinuz, signed modules, modules.*).
    pub modules: PathBuf,
    pub vmlinuz: PathBuf,
    #[serde(default)]
    pub initrd: Option<PathBuf>,
    /// `~/.cache/tilo-kernel/<linux version>/`.
    pub cache: PathBuf,
    pub builder_image: String,
    pub jobs: u32,
    pub build_seconds: f64,
    pub compile_seconds: f64,
    pub log: String,
}

/// `bash <script> --result <file> [--jobs N] [--direct-initrd]`.
pub fn build_command(script: &str, result: &Path, jobs: Option<u32>, direct: bool) -> CommandLine {
    let mut command =
        CommandLine::new("bash", &[script, "--result"]).arg(result.display().to_string());
    if let Some(jobs) = jobs {
        command = command.arg("--jobs").arg(jobs.to_string());
    }
    if direct {
        command = command.arg("--direct-initrd");
    }
    command
}

/// The running kernel's command line, for a directly booted kernel: without
/// the words a boot loader adds.
pub fn direct_cmdline(cmdline: &str) -> String {
    cmdline
        .split_whitespace()
        .filter(|w| !w.starts_with("BOOT_IMAGE=") && !w.starts_with("initrd="))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Where a kernel layer is synced in the VM before import: `<dir>/usr/lib/modules/`.
pub fn vm_staging_dir(worktree_id: &str) -> String {
    format!("/var/lib/tilo-vm/kernel/{worktree_id}")
}

/// rsync the staged module tree into `<staging>/usr/lib/modules/`, sending
/// only what changed since the last swap from this worktree.
pub fn rsync_modules(
    built: &Built,
    endpoint: &SshEndpoint,
    key: Option<&str>,
    staging: &str,
) -> Result<CommandLine, CliError> {
    let rsh = vmbuild::rsync_rsh(endpoint, key)?;
    let dest = format!("{staging}/usr/lib/modules");
    let source = built
        .modules
        .parent()
        .map(|p| format!("{}/", p.display()))
        .unwrap_or_default();
    Ok(CommandLine::new(
        &crate::host::rsync_program().unwrap_or_else(|| "rsync".into()),
        &["--archive", "--delete", "--checksum", "--compress"],
    )
    .arg("--rsh")
    .arg(rsh)
    .arg("--rsync-path")
    .arg(format!("mkdir -p {dest} && rsync"))
    .arg(source)
    .arg(format!("{}@{}:{dest}/", endpoint.user, endpoint.host)))
}

/// Pack the staged module tree as a layer: `usr/lib/modules/<kver>/`.
///
/// # Errors
/// I/O failures.
pub fn pack(built: &Built, out: &Path) -> Result<(), CliError> {
    let file =
        std::fs::File::create(out).map_err(CliError::io(format!("create {}", out.display())))?;
    let mut builder = tar::Builder::new(file);
    builder.follow_symlinks(false);
    builder
        .append_dir_all(format!("usr/lib/modules/{}", built.kver), &built.modules)
        .map_err(CliError::io(format!("pack {}", built.modules.display())))?;
    builder
        .into_inner()
        .map(drop)
        .map_err(CliError::io("finish kernel layer"))
}

/// What identifies a kernel layer: the worktree's commit and dirty state,
/// the architecture, and (as `features`) the release and builder image.
pub fn provenance(worktree: &Worktree, built: &Built, arch: &str) -> Provenance {
    let mut provenance = build::source_provenance(worktree, "kernel", "host");
    // Not a cargo build: the image's Cargo.lock says nothing about it.
    provenance.cargo_lock_hash = None;
    provenance.target = Some(format!("{arch}-unknown-linux-gnu"));
    provenance.features = vec![
        format!("kernel-release={}", built.kver),
        format!("builder-image={}", built.builder_image),
    ];
    provenance
}

/// `osrelease` and `version` read back over SSH, each on its own line.
pub fn parse_running(text: &str) -> Option<(String, String)> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    Some((lines.next()?.to_owned(), lines.next()?.to_owned()))
}

fn seconds(since: Instant) -> f64 {
    since.elapsed().as_secs_f64()
}

impl App {
    /// `tilo-vm swap kernel`.
    pub(super) fn swap_kernel(
        &mut self,
        slot: Option<&str>,
        component: &Component,
        from: &str,
        flags: KernelFlags,
    ) -> Result<Outcome, CliError> {
        let KernelFlags {
            direct,
            jobs,
            cmdline,
        } = flags;
        if let Some(slot) = slot.filter(|s| *s != VM_SLOT) {
            return Err(CliError::ScopeMismatch {
                component: component.name.clone(),
                slot: slot.to_owned(),
                scope: "vm".into(),
            });
        }
        self.receipt.set("slot", VM_SLOT);
        self.receipt.set("component", &component.name);
        let started = Instant::now();
        if let Some(hash) = from.strip_prefix("layer:") {
            if direct {
                return Err(CliError::Unsupported {
                    what: "--direct with --from layer:".into(),
                    alternative:
                        "a direct boot takes a fresh host build: tilo-vm swap kernel --direct"
                            .into(),
                });
            }
            let vm = self.resolve_vm(None)?;
            let mut remote = self.connect(&vm)?;
            return self.install_kernel_layer(&vm, &mut remote, component, hash, None, started);
        }
        if from != "worktree" {
            return Err(CliError::Unsupported {
                what: format!("--from {from} for the kernel"),
                alternative: "build it from this worktree (tilo-vm swap kernel), or reuse a layer already in the VM: --from layer:<hash>. CI publishes the kernel as RPMs inside the image, not as a layer".into(),
            });
        }
        if !cfg!(target_os = "linux") {
            return Err(CliError::Unsupported {
                what: format!("kernel builds on {}", std::env::consts::OS),
                alternative: "build the kernel on a Linux host with rootless podman (os/kernel/dev-build.sh), then swap from there".into(),
            });
        }
        if !on_path("podman") {
            return Err(CliError::ToolMissing {
                tool: "podman".into(),
                purpose: "run the kernel builder image (os/kernel/dev-build.sh)".into(),
            });
        }
        let worktree = self.worktree.clone().ok_or(CliError::NotInWorktree)?;
        let vm = self.resolve_vm(None)?;
        if direct {
            let adapter = Self::adapter(&vm)?;
            if !adapter.capabilities().direct_kernel {
                return Err(CliError::Unsupported {
                    what: format!("direct kernel boot with the {} adapter", adapter.name()),
                    alternative: "install the kernel in the VM instead: tilo-vm swap kernel".into(),
                });
            }
            // The command line comes from the running VM, before anything is built.
            let cmdline = match cmdline {
                Some(cmdline) => cmdline,
                None => self.vm_kernel_facts(&vm)?.0,
            };
            let built = self.build_kernel(&worktree, component, jobs, true)?;
            return self.boot_direct(&vm, &built, &cmdline, started);
        }
        // Connect first: an unreachable VM should fail before a long build.
        let mut remote = self.connect(&vm)?;
        self.check_vm_space(&mut remote, &vm)?;
        drop(remote);
        let built = self.build_kernel(&worktree, component, jobs, false)?;
        let mut remote = self.connect(&vm)?;
        let layer = self.upload_kernel(&vm, &mut remote, &worktree, &built)?;
        self.install_kernel_layer(&vm, &mut remote, component, &layer, Some(&built), started)
    }

    /// Run the manifest's `dev_build` script; its output streams here.
    fn build_kernel(
        &mut self,
        worktree: &Worktree,
        component: &Component,
        jobs: Option<u32>,
        direct: bool,
    ) -> Result<Built, CliError> {
        let script = component
            .dev_build
            .clone()
            .ok_or_else(|| CliError::Unsupported {
                what: format!("building '{}' for a dev VM", component.name),
                alternative: "its os/components.toml entry names no dev_build script".into(),
            })?;
        let staging = build::Staging::new(worktree)?;
        let result = staging.tree().with_extension("json");
        let command = build_command(&script, &result, jobs, direct);
        eprintln!("tilo-vm: building the kernel: {}", command.display());
        self.runner.cwd = Some(worktree.root.clone());
        let runner = &mut self.runner;
        let ok = self
            .receipt
            .phase("build", || runner.interactive(&command))?;
        let log = std::fs::read_to_string(&result)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|v| v.get("log").and_then(Value::as_str).map(str::to_owned));
        if !ok {
            return Err(CliError::KernelBuild {
                log: log.unwrap_or_else(|| "~/.cache/tilo-kernel/<version>/build.log".into()),
            });
        }
        let text = std::fs::read_to_string(&result)
            .map_err(CliError::io(format!("read {}", result.display())))?;
        let built: Built = serde_json::from_str(&text).map_err(|e| CliError::Config {
            path: result.display().to_string(),
            detail: e.to_string(),
        })?;
        self.receipt.set("kernel", &built.kver);
        self.receipt.record("compile", built.compile_seconds);
        Ok(built)
    }

    /// Get the staged modules into the VM and import them as a layer.
    fn upload_kernel(
        &mut self,
        vm: &Resolved,
        remote: &mut Remote,
        worktree: &Worktree,
        built: &Built,
    ) -> Result<String, CliError> {
        let provenance = provenance(worktree, built, &remote.hello.arch);
        let source = if remote.hello.tools.rsync && on_path("rsync") {
            let endpoint = self.endpoint(vm)?;
            let id = vmbuild::worktree_id(worktree, crate::context::hostname().as_deref());
            let staging = vm_staging_dir(&id);
            let rsync = rsync_modules(built, &endpoint, vm.entry.key.as_deref(), &staging)?;
            eprintln!(
                "tilo-vm: syncing {} to {}:{staging}",
                built.modules.display(),
                vm.name
            );
            let runner = &mut self.runner;
            if !self
                .receipt
                .phase("transfer", || runner.interactive(&rsync))?
            {
                return Err(CliError::CommandFailed {
                    command: rsync.display(),
                    status: "sync failed".into(),
                    stderr: "rsync's output above says why".into(),
                });
            }
            LayerSource::Path(staging)
        } else {
            let staging = build::Staging::new(worktree)?;
            let archive = staging.archive();
            pack(built, &archive)?;
            let size = std::fs::metadata(&archive).map_or(0, |m| m.len());
            eprintln!(
                "tilo-vm: uploading the kernel layer ({})",
                human_bytes(size)
            );
            let upload = self
                .receipt
                .phase("transfer", || -> Result<String, CliError> {
                    let file = std::fs::File::open(&archive)
                        .map_err(CliError::io(format!("open {}", archive.display())))?;
                    remote.upload(std::io::BufReader::new(file))
                })?;
            LayerSource::Upload(upload)
        };
        let imported = self.receipt.phase("import", || {
            remote.call(LayerImport {
                source,
                provenance: Box::new(provenance),
            })
        })?;
        Ok(imported.hash)
    }

    /// Swap `layer` into the vm slot, reboot onto it and confirm.
    fn install_kernel_layer(
        &mut self,
        vm: &Resolved,
        remote: &mut Remote,
        component: &Component,
        layer: &str,
        built: Option<&Built>,
        started: Instant,
    ) -> Result<Outcome, CliError> {
        self.receipt.set("layer", layer);
        let applied = Instant::now();
        let result = self.receipt.phase("apply", || {
            remote.call(Swap {
                slot: VM_SLOT.into(),
                component: component.name.clone(),
                layer: layer.to_owned(),
                info: Some(component.info.clone()),
            })
        })?;
        self.warnings.extend(result.warnings.iter().cloned());
        let apply_seconds = seconds(applied);
        if !result.changed {
            return Ok(Outcome::new(
                format!(
                    "the vm slot already runs kernel layer {}; nothing to do",
                    short(layer)
                ),
                json!({ "apply": result }),
            )
            .next(&["tilo-vm vm ssh -- uname -a", "tilo-vm rollback --slot vm"]));
        }
        let expect = built.map(|b| b.uts_version.as_str());
        let (confirm, reboot_seconds) = self.reboot_and_confirm(vm, &result, expect)?;
        Ok(Self::kernel_outcome(
            vm,
            built,
            &result,
            &confirm,
            apply_seconds,
            reboot_seconds,
            started,
        ))
    }

    /// Reboot the VM, wait for SSH on a new boot, and confirm the kernel.
    fn reboot_and_confirm(
        &mut self,
        vm: &Resolved,
        result: &ApplyResult,
        expect_version: Option<&str>,
    ) -> Result<(BootConfirmResult, f64), CliError> {
        let expected = result
            .boot
            .as_ref()
            .and_then(|b| b.kver.clone())
            .unwrap_or_else(|| "the image's kernel".into());
        let adapter = Self::adapter(vm)?;
        let endpoint = adapter.ssh_endpoint(&vm.vm(), &mut self.runner)?;
        let key = vm.entry.key.clone();
        let boot_id = self.remote_text(
            &endpoint,
            key.as_deref(),
            "cat /proc/sys/kernel/random/boot_id",
        );
        eprintln!("tilo-vm: rebooting VM {} onto {expected}", vm.name);
        let reboot = remote::ssh_command(
            &endpoint,
            key.as_deref(),
            &["systemctl".into(), "--no-block".into(), "reboot".into()],
            false,
        );
        let _ = self.runner.output(&reboot);
        let rebooted = Instant::now();
        let mut up = self.wait_for_new_boot(&endpoint, key.as_deref(), boot_id.as_deref());
        let mut detail = String::new();
        if !up {
            detail = format!(
                "no SSH answer within {}s of the reboot",
                REBOOT_TIMEOUT.as_secs()
            );
            if let Some(reset) = adapter.reset(&vm.vm()) {
                eprintln!(
                    "tilo-vm: {detail}; resetting VM {} onto its default boot entry",
                    vm.name
                );
                let _ = self.runner.output(&reset);
                detail.push_str("; the VM was reset onto its default entry");
                up = self.wait_for_new_boot(&endpoint, key.as_deref(), None);
            }
        }
        let reboot_seconds = seconds(rebooted);
        self.receipt.record("reboot", reboot_seconds);
        if !up {
            return Err(CliError::Unreachable {
                vm: vm.name.clone(),
                detail: format!("{detail}; it did not come back"),
                debug: Self::debug_commands(adapter.as_ref(), &vm.name),
            });
        }
        let mut remote = self.connect(vm)?;
        let confirm = self.receipt.phase("confirm", || {
            remote.call(BootConfirm {
                expect_version: expect_version.map(str::to_owned),
            })
        })?;
        if !confirm.confirmed {
            if detail.is_empty() {
                detail = match expect_version {
                    Some(version) if confirm.running == expected => {
                        format!(
                            "it reports {} rather than the new build's {version}",
                            confirm.version
                        )
                    }
                    _ => "the new entry was booted once and the VM came up on another".into(),
                };
            }
            return Err(CliError::KernelNotRunning {
                vm: vm.name.clone(),
                expected,
                running: confirm.running,
                detail,
            });
        }
        Ok((confirm, reboot_seconds))
    }

    /// Poll until SSH answers with a boot id other than `old`.
    fn wait_for_new_boot(
        &mut self,
        endpoint: &SshEndpoint,
        key: Option<&str>,
        old: Option<&str>,
    ) -> bool {
        let started = Instant::now();
        // Give the old boot time to go down before polling it.
        std::thread::sleep(Duration::from_secs(3));
        while started.elapsed() < REBOOT_TIMEOUT {
            if let Some(id) = self.remote_text(endpoint, key, "cat /proc/sys/kernel/random/boot_id")
                && Some(id.as_str()) != old
            {
                return true;
            }
            std::thread::sleep(Duration::from_secs(2));
        }
        false
    }

    /// One command's trimmed stdout over SSH, when it succeeds.
    fn remote_text(
        &mut self,
        endpoint: &SshEndpoint,
        key: Option<&str>,
        command: &str,
    ) -> Option<String> {
        let line = remote::ssh_command(endpoint, key, &[command.to_owned()], false);
        let output = self.runner.output(&line).ok()?;
        output.success.then(|| output.stdout.trim().to_owned())
    }

    /// The VM's kernel command line and running release, over SSH.
    fn vm_kernel_facts(&mut self, vm: &Resolved) -> Result<(String, String), CliError> {
        let adapter = Self::adapter(vm)?;
        let endpoint = adapter.ssh_endpoint(&vm.vm(), &mut self.runner)?;
        let text = self
            .remote_text(
                &endpoint,
                vm.entry.key.as_deref(),
                "cat /proc/cmdline /proc/sys/kernel/osrelease",
            )
            .ok_or_else(|| CliError::Unreachable {
                vm: vm.name.clone(),
                detail: "a direct boot takes the running VM's kernel command line, and SSH did not answer; pass it with --cmdline".into(),
                debug: Self::debug_commands(adapter.as_ref(), &vm.name),
            })?;
        let (cmdline, release) = parse_running(&text).unwrap_or_default();
        Ok((direct_cmdline(&cmdline), release))
    }

    /// Point the VM at the host-built kernel and restart it.
    fn boot_direct(
        &mut self,
        vm: &Resolved,
        built: &Built,
        cmdline: &str,
        started: Instant,
    ) -> Result<Outcome, CliError> {
        let initrd = built.initrd.clone().ok_or_else(|| CliError::KernelBuild {
            log: built.log.clone(),
        })?;
        // Stable copies per VM: the next build rewrites the cache while this
        // VM may still be running from them.
        let dir = built
            .cache
            .parent()
            .unwrap_or(&built.cache)
            .join("direct")
            .join(&vm.name);
        std::fs::create_dir_all(&dir).map_err(CliError::io(format!("create {}", dir.display())))?;
        let kernel = dir.join("vmlinuz");
        let initramfs = dir.join("initrd.img");
        for (from, to) in [(&built.vmlinuz, &kernel), (&initrd, &initramfs)] {
            std::fs::copy(from, to).map_err(CliError::io(format!("copy {}", from.display())))?;
        }
        let direct = DirectKernel {
            kernel: kernel.display().to_string(),
            initrd: initramfs.display().to_string(),
            cmdline: cmdline.to_owned(),
            loader_partuuid: None,
            kver: built.kver.clone(),
        };
        let (confirm, reboot_seconds) = self.redefine_and_restart(vm, Some(direct))?;
        let (running, version) = confirm;
        if running != built.kver || version != built.uts_version {
            return Err(CliError::KernelNotRunning {
                vm: vm.name.clone(),
                expected: built.kver.clone(),
                running,
                detail: format!("directly booted, it reports {version}"),
            });
        }
        let text = format!(
            "kernel {} is running in VM {} by direct boot ({version})\n  build {:.0}s (compile and stage {:.0}s, {} jobs), boot {reboot_seconds:.0}s, {:.0}s in all\n  booted from {} with a generic initramfs; nothing was installed in the VM: its modules outside the initramfs are absent\n  tilo-vm rollback --slot vm boots the VM's disk again",
            built.kver,
            vm.name,
            built.build_seconds,
            built.compile_seconds,
            built.jobs,
            seconds(started),
            dir.display(),
        );
        Ok(Outcome::new(
            text,
            json!({ "vm": vm.name, "kver": built.kver, "version": version, "direct": true, "build": built, "boot_seconds": reboot_seconds, "seconds": seconds(started) }),
        )
        .next(&["tilo-vm vm ssh -- uname -a", "tilo-vm vm serial", "tilo-vm rollback --slot vm"]))
    }

    /// Store `direct` in the registry, redefine the VM from it and restart
    /// it; returns the running release and version once SSH answers.
    fn redefine_and_restart(
        &mut self,
        vm: &Resolved,
        direct: Option<DirectKernel>,
    ) -> Result<((String, String), f64), CliError> {
        let mut registry = self.registry()?;
        let entry = registry
            .vms
            .get_mut(&vm.name)
            .ok_or_else(|| CliError::UnknownVm {
                name: vm.name.clone(),
                registry: self.registry_path.display().to_string(),
                known: Vec::new(),
            })?;
        entry.direct_kernel = direct;
        let updated = Resolved {
            name: vm.name.clone(),
            entry: entry.clone(),
            source: vm.source.clone(),
        };
        let key = vm.entry.key.clone().unwrap_or_else(|| {
            sshkey::key_path(&self.registry_path, &vm.name)
                .display()
                .to_string()
        });
        let root_key = sshkey::read_public(Path::new(&key))?;
        let adapter = Self::adapter(&updated)?;
        let define = adapter.redefine(&updated.vm(), &root_key, &mut self.runner)?;
        run_all(&mut self.runner, &define)?;
        // Recorded once the hypervisor holds the new definition.
        registry.save(&self.registry_path)?;
        if let Some(off) = adapter.power_off(&updated.vm()) {
            // Fails when the VM is already off, which is fine.
            let _ = self.runner.output(&off);
        }
        run_all(&mut self.runner, &adapter.start(&updated.vm()))?;
        let endpoint = adapter.ssh_endpoint(&updated.vm(), &mut self.runner)?;
        let booted = Instant::now();
        let up = self.wait_for_new_boot(&endpoint, updated.entry.key.as_deref(), None);
        let boot_seconds = seconds(booted);
        self.receipt.record("boot", boot_seconds);
        let running = up
            .then(|| {
                self.remote_text(
                    &endpoint,
                    updated.entry.key.as_deref(),
                    "cat /proc/sys/kernel/osrelease /proc/sys/kernel/version",
                )
            })
            .flatten()
            .and_then(|t| parse_running(&t))
            .ok_or_else(|| CliError::Unreachable {
                vm: vm.name.clone(),
                detail: format!(
                    "no SSH answer within {}s of the restart",
                    REBOOT_TIMEOUT.as_secs()
                ),
                debug: Self::debug_commands(adapter.as_ref(), &vm.name),
            })?;
        Ok((running, boot_seconds))
    }

    fn kernel_outcome(
        vm: &Resolved,
        built: Option<&Built>,
        result: &ApplyResult,
        confirm: &BootConfirmResult,
        apply_seconds: f64,
        reboot_seconds: f64,
        started: Instant,
    ) -> Outcome {
        let boot = result.boot.as_ref();
        let mut text = format!(
            "kernel {} is running in VM {} ({})",
            confirm.running, vm.name, confirm.version
        );
        if let Some(built) = built {
            let _ = write!(
                text,
                "\n  build {:.0}s (compile and stage {:.0}s, {} jobs)",
                built.build_seconds, built.compile_seconds, built.jobs
            );
        }
        let initramfs = boot.and_then(|b| b.initramfs.as_deref()).map_or_else(
            || "initramfs reused".to_owned(),
            |how| {
                format!(
                    "initramfs {:.0}s: {how}",
                    boot.map_or(0.0, |b| b.initramfs_seconds)
                )
            },
        );
        let _ = write!(
            text,
            "\n  install {apply_seconds:.0}s ({initramfs}), reboot {reboot_seconds:.0}s, {:.0}s in all",
            seconds(started)
        );
        if let Some(entry) = &confirm.default_entry {
            let _ = write!(text, "\n  default boot entry: {entry}");
        }
        if let Some(fallback) = boot.and_then(|b| b.fallback.as_deref()) {
            let _ = write!(text, " (fallback before this swap: {fallback})");
        }
        let _ = write!(
            text,
            "\n  vm slot generation {}; tilo-vm rollback --slot vm returns to the previous kernel",
            result.generation
        );
        Outcome::new(
            text,
            json!({
                "vm": vm.name,
                "kver": confirm.running,
                "version": confirm.version,
                "build": built,
                "apply": result,
                "confirm": confirm,
                "apply_seconds": apply_seconds,
                "reboot_seconds": reboot_seconds,
                "seconds": seconds(started),
            }),
        )
        .next(&[
            "tilo-vm vm ssh -- uname -a",
            "tilo-vm rollback --slot vm",
            "tilo-vm guide kernel",
        ])
    }

    /// `tilo-vm rollback --slot vm`: off a direct boot, or the vm slot's
    /// previous generation, rebooting when that changes the kernel.
    pub(super) fn rollback_vm(&mut self, to: Option<u64>) -> Result<Outcome, CliError> {
        let vm = self.resolve_vm(None)?;
        let started = Instant::now();
        if let Some(direct) = vm.entry.direct_kernel.clone() {
            let ((running, version), boot_seconds) = self.redefine_and_restart(&vm, None)?;
            return Ok(Outcome::new(
                format!(
                    "VM {} boots its own disk again: kernel {running} ({version}); the direct kernel {} is off\n  boot {boot_seconds:.0}s; the vm slot's generations are unchanged",
                    vm.name, direct.kver
                ),
                json!({ "vm": vm.name, "kver": running, "version": version, "direct": false, "seconds": seconds(started) }),
            )
            .next(&["tilo-vm vm ssh -- uname -a", "tilo-vm status"]));
        }
        let mut remote = self.connect(&vm)?;
        let applied = Instant::now();
        let result = self.receipt.phase("apply", || {
            remote.call(Rollback {
                slot: VM_SLOT.into(),
                to,
            })
        })?;
        drop(remote);
        if !(result.changed && result.policy == Policy::Reboot) {
            return Ok(Outcome::new(describe_apply(&result), json!(result)).next(&[
                "tilo-vm shot",
                "tilo-vm logs",
                "tilo-vm status",
            ]));
        }
        let apply_seconds = seconds(applied);
        let (confirm, reboot_seconds) = self.reboot_and_confirm(&vm, &result, None)?;
        Ok(Self::kernel_outcome(
            &vm,
            None,
            &result,
            &confirm,
            apply_seconds,
            reboot_seconds,
            started,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn built() -> Built {
        Built {
            kver: "7.2.5-200.fc44.x86_64+dev".into(),
            uts_version: "#3 SMP PREEMPT_DYNAMIC Thu Jan  1 00:00:00 UTC 1970".into(),
            modules: PathBuf::from(
                "/c/tilo-kernel/linux-7.2.5/out/stage/lib/modules/7.2.5-200.fc44.x86_64+dev",
            ),
            vmlinuz: PathBuf::from("/c/vmlinuz"),
            initrd: None,
            cache: PathBuf::from("/c/tilo-kernel/linux-7.2.5"),
            builder_image: "localhost/tilo-kernel-builder:x86_64-0123".into(),
            jobs: 8,
            build_seconds: 90.0,
            compile_seconds: 80.0,
            log: "/c/tilo-kernel/linux-7.2.5/build.log".into(),
        }
    }

    /// The build runs the manifest script, the result file parses, modules sync into the layer shape, a
    /// packed layer holds one release under /usr, and direct boots take the running command line.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn kernel_builds_run_the_manifest_and_pack_one_release_layer() {
        // Phase: the_build_runs_the_manifest_script.
        {
            let command = build_command(
                "os/kernel/dev-build.sh",
                Path::new("/t/r.json"),
                Some(4),
                true,
            );
            assert_eq!(
                command.display(),
                "bash os/kernel/dev-build.sh --result /t/r.json --jobs 4 --direct-initrd"
            );
            assert_eq!(
                build_command(
                    "os/kernel/dev-build.sh",
                    Path::new("/t/r.json"),
                    None,
                    false
                )
                .display(),
                "bash os/kernel/dev-build.sh --result /t/r.json"
            );
        }

        // Phase: the_result_file_parses.
        {
            let json = serde_json::to_string(&built()).unwrap();
            let back: Built = serde_json::from_str(&json).unwrap();
            assert_eq!(back.kver, "7.2.5-200.fc44.x86_64+dev");
            let minimal: Built = serde_json::from_str(
                r##"{"kver":"k","uts_version":"#1","modules":"/m","vmlinuz":"/m/vmlinuz","initrd":null,
                    "cache":"/c","builder_image":"i","builder_image_id":"x","jobs":8,
                    "build_seconds":1,"compile_seconds":1,"log":"/c/build.log"}"##,
            )
            .unwrap();
            assert!(minimal.initrd.is_none());
        }

        // Phase: modules_sync_into_the_layer_shape.
        {
            let endpoint = SshEndpoint {
                user: "root".into(),
                host: "127.0.0.1".into(),
                port: 2223,
            };
            let command = rsync_modules(
                &built(),
                &endpoint,
                Some("/k/id"),
                &vm_staging_dir("wt-1a2b"),
            )
            .unwrap();
            let text = command.display();
            assert_eq!(
                command.program,
                crate::host::rsync_program().unwrap_or_else(|| "rsync".into())
            );
            assert!(
                text.contains("--archive --delete --checksum --compress"),
                "{text}"
            );
            assert!(
                text.contains("mkdir -p /var/lib/tilo-vm/kernel/wt-1a2b/usr/lib/modules && rsync"),
                "{text}"
            );
            assert!(
                text.contains(" /c/tilo-kernel/linux-7.2.5/out/stage/lib/modules/ root@127.0.0.1:/var/lib/tilo-vm/kernel/wt-1a2b/usr/lib/modules/"),
                "{text}"
            );
            assert!(text.contains("-p 2223"), "{text}");
        }

        // Phase: a_packed_layer_holds_one_release_under_usr.
        {
            let dir = tempfile::tempdir().unwrap();
            let modules = dir.path().join("lib/modules/7.2+dev");
            std::fs::create_dir_all(modules.join("kernel")).unwrap();
            std::fs::write(modules.join("vmlinuz"), b"k").unwrap();
            std::fs::write(modules.join("kernel/a.ko.zst"), b"m").unwrap();
            let mut b = built();
            b.kver = "7.2+dev".into();
            b.modules = modules;
            let out = dir.path().join("layer.tar");
            pack(&b, &out).unwrap();
            let mut archive = tar::Archive::new(std::fs::File::open(&out).unwrap());
            let paths: Vec<String> = archive
                .entries()
                .unwrap()
                .map(|e| e.unwrap().path().unwrap().display().to_string())
                .collect();
            assert!(
                paths.iter().any(|p| p == "usr/lib/modules/7.2+dev/vmlinuz"),
                "{paths:?}"
            );
            assert!(
                paths
                    .iter()
                    .any(|p| p == "usr/lib/modules/7.2+dev/kernel/a.ko.zst"),
                "{paths:?}"
            );
            assert!(
                paths
                    .iter()
                    .all(|p| p.starts_with("usr/lib/modules/7.2+dev")),
                "{paths:?}"
            );
        }

        // Phase: direct_boots_take_the_running_command_line.
        {
            assert_eq!(
                direct_cmdline(
                    "BOOT_IMAGE=(hd0,gpt2)/vmlinuz root=UUID=1 ro initrd=/x console=ttyS0"
                ),
                "root=UUID=1 ro console=ttyS0"
            );
            assert_eq!(
                parse_running("7.2+dev\n#3 SMP PREEMPT_DYNAMIC x\n"),
                Some(("7.2+dev".into(), "#3 SMP PREEMPT_DYNAMIC x".into()))
            );
            assert_eq!(parse_running("only\n"), None);
        }
    }
}
