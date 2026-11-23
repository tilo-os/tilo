//! A real machine of yours registered by its device key. `vm add` asks the device's own daemon,
//! as this machine's device of your identity, checks what the other commands rely on, and
//! registers it once it answers.

use std::fmt::Write as _;
use std::net::{IpAddr, SocketAddr};

use serde_json::json;
use tilo_device_protocol::StatusResult;

use super::{App, Outcome, Resolved, absolute, describe_tools};
use crate::cli::AddArgs;
use crate::config::{DAEMON_PORT, DeviceRoute, Source, VmEntry};
use crate::doctor::Check;
use crate::error::CliError;
use crate::hypervisor::{DEVICE, SshEndpoint};

pub fn device_checks(answered: Result<&StatusResult, &CliError>) -> Vec<Check> {
    let status = match answered {
        Ok(status) => status,
        Err(error) => return vec![Check::fail("device", error.to_string(), error.hint())],
    };
    let mut checks = vec![Check::ok(
        "device",
        format!(
            "answers this machine: device steps {} on {}",
            status.daemon_version, status.arch
        ),
    )];
    // A device takes CI layers from this host, never pulling the image itself.
    let tools = describe_tools(&status.tools, true);
    checks.push(if status.tools.can_build() {
        Check::ok("toolchain", tools)
    } else {
        Check::warn(
            "toolchain",
            tools,
            "swaps from this worktree build on the device and need cargo and python3: move it onto a current board dev image (tilo-vm vm upgrade), or swap CI builds (--from sha:<commit>)",
        )
    });
    for issue in &status.issues {
        checks.push(Check::warn(
            "accounts",
            format!("account {}: {}", issue.account, issue.problem),
            issue.hint.clone(),
        ));
    }
    let image = "install a Tilo dev image on it (tilo-vm guide devices: a board dev image, or the dev disk)";
    checks.push(if status.boot.bootc.is_some() {
        Check::ok(
            "bootc",
            match &status.boot.board {
                Some(board) => format!(
                    "vm upgrade --from sha:<commit> takes the {board} board image of that build"
                ),
                None => "vm upgrade --from sha:<commit> takes the dev image of that build".into(),
            },
        )
    } else {
        Check::warn(
            "bootc",
            "no bootc: vm upgrade cannot move this machine onto another image",
            image,
        )
    });
    checks
}

pub fn usable(checks: &[Check]) -> bool {
    checks
        .iter()
        .filter(|c| c.check == "device")
        .all(|c| c.state != "fail")
}

impl App {
    pub(super) fn vm_add(&mut self, args: &AddArgs) -> Result<Outcome, CliError> {
        let device = args.device.to_ascii_lowercase();
        if device.len() != 64 || !device.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(CliError::InvalidInput(format!(
                "--device {}: a device key is 64 hex digits, as tilo device list shows it",
                args.device
            )));
        }
        if let Some(ssh) = &args.ssh
            && SshEndpoint::parse(ssh).is_none()
        {
            return Err(CliError::Config {
                path: "--ssh".into(),
                detail: format!("'{ssh}' is not [user@]host[:port]"),
            });
        }
        let key = ssh_key(args)?;
        let mut registry = self.registry()?;
        if let Some(existing) = registry.vms.get(&args.name)
            && existing.hypervisor != DEVICE
        {
            return Err(CliError::InvalidInput(format!(
                "'{}' is already a {} VM in {}; pick another name, or tilo-vm vm rm {} first",
                args.name,
                existing.hypervisor,
                self.registry_path.display(),
                args.name
            )));
        }
        let machine = self.machine()?;
        let addresses = self.daemon_address(args.address.as_deref());
        let mut entry = VmEntry {
            hypervisor: DEVICE.into(),
            ssh: args.ssh.clone(),
            key,
            device: Some(DeviceRoute {
                device,
                workspace: machine.workspace,
                addresses,
                relay: None,
            }),
            ..VmEntry::default()
        };
        let vm = Resolved {
            name: args.name.clone(),
            entry: entry.clone(),
            source: Source::Argument,
        };
        let mut connected = self.connect(&vm);
        let checks = device_checks(connected.as_ref().map(|remote| &remote.status));
        let ok = usable(&checks);
        let mut text = checks_text(&args.name, &checks);
        if let Ok(remote) = &mut connected {
            entry.arch = Some(remote.status.arch.clone());
            if let Some(facts) = Self::net_facts(remote, &vm) {
                super::reach::apply_net(&mut entry, &facts);
            }
            if let Some(wake) = &entry.wake {
                let _ = write!(text, "\n  wake: {}", wake.describe());
            }
        }
        let data = json!({
            "vm": args.name,
            "device": entry.device,
            "arch": entry.arch,
            "registered": ok,
            "checks": checks,
            "hostname": entry.hostname,
            "mac": entry.mac,
            "wake": entry.wake,
        });
        if !ok {
            text.push_str("\nnot registered: fix the failed check above, then rerun this command");
            let mut outcome = Outcome::new(text, data);
            outcome.failed = true;
            return Ok(outcome);
        }
        let changed = registry.vms.get(&args.name) != Some(&entry);
        registry.vms.insert(args.name.clone(), entry);
        if registry.default.is_none() {
            registry.default = Some(args.name.clone());
        }
        registry.save(&self.registry_path)?;
        let _ = write!(
            text,
            "\n{} {} in {}",
            if changed {
                "registered"
            } else {
                "already registered as"
            },
            args.name,
            self.registry_path.display()
        );
        self.warnings.clear();
        let use_it = format!("tilo-vm vm use {}", args.name);
        Ok(Outcome::new(text, data).next(&[
            use_it.as_str(),
            "tilo-vm account create --headless",
            "tilo-vm guide devices",
        ]))
    }
}

/// `--key`, made absolute, once it names a file.
fn ssh_key(args: &AddArgs) -> Result<Option<String>, CliError> {
    match &args.key {
        Some(key) if !key.is_file() => Err(CliError::InvalidInput(format!(
            "--key {}: no such file",
            key.display()
        ))),
        Some(key) => Ok(Some(absolute(key).display().to_string())),
        None => Ok(None),
    }
}

impl App {
    /// `--address` as the daemon's socket address: as given with a port, else at the usual
    /// daemon port of the IP it is or resolves to.
    fn daemon_address(&mut self, address: Option<&str>) -> Vec<String> {
        address
            .and_then(|address| {
                address.parse::<SocketAddr>().ok().or_else(|| {
                    address
                        .parse::<IpAddr>()
                        .ok()
                        .or_else(|| crate::reach::resolve(&mut self.runner, address))
                        .map(|ip| SocketAddr::new(ip, DAEMON_PORT))
                })
            })
            .map(|address| address.to_string())
            .into_iter()
            .collect()
    }
}

fn checks_text(name: &str, checks: &[Check]) -> String {
    let mut text = format!("device {name}");
    for check in checks {
        let _ = write!(
            text,
            "\n  {:<4} {:<10} {}",
            check.state, check.check, check.detail
        );
        if let Some(fix) = &check.fix {
            let _ = write!(text, "\n       fix: {fix}");
        }
    }
    text
}
