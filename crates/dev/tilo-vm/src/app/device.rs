//! A device reached as its owner's account rather than as root.
//!
//! The installer's dev SSH setup lets only the owner's account in
//! (`PermitRootLogin no`, `AllowUsers <owner>`), so on such a device tilo-vm
//! logs in as that account and runs every remote command through `sudo -n`
//! ([`crate::remote::as_root`]). `vm add --ssh <user>@<host>` checks what the
//! other commands rely on (SSH, root through sudo, tilo-vmd and its protocol;
//! the toolchain and bootc for builds and upgrades), names each fix, and
//! registers the device only once the required ones pass. `--grant-sudo`
//! writes the one sudoers drop-in that allows it, with the user's password
//! typed on this terminal.

use std::fmt::Write as _;

use serde_json::json;
use tilo_vm_protocol::HelloResult;

use super::{App, Outcome, absolute, describe_tools};
use crate::cli::AddArgs;
use crate::config::VmEntry;
use crate::doctor::Check;
use crate::error::CliError;
use crate::hypervisor::{CommandLine, DEVICE, Runner as _, SshEndpoint};
use crate::remote::{self, Remote};
use crate::vmbuild::sh_quote;

/// What `add-device` reads as the SSH user, before anything runs as root.
/// The login shell may be any shell, so the script runs under `sh -c`.
pub const PROBE: &str = r#"echo "user=$(id -un)"; if [ "$(id -u)" = 0 ]; then echo sudo=root; elif out=$(sudo -n true 2>&1); then echo sudo=ok; else echo "sudo=no $(printf %s "$out" | head -n 1)"; fi; if command -v tilo-vmd >/dev/null 2>&1; then echo tilo-vmd=yes; else echo tilo-vmd=no; fi; if command -v bootc >/dev/null 2>&1; then echo bootc=yes; else echo bootc=no; fi; echo "arch=$(uname -m)"; echo @@board; cat /usr/lib/tilo/hardware-profile.json 2>/dev/null"#;

/// [`PROBE`]'s answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub user: Option<String>,
    /// `Ok(true)`: root itself; `Ok(false)`: passwordless sudo; `Err`: why not.
    pub root: Result<bool, String>,
    pub tilo_vmd: bool,
    pub bootc: bool,
    pub arch: Option<String>,
    /// The board the image was built for, on a board dev image.
    pub board: Option<String>,
}

pub fn parse_probe(output: &str) -> Probe {
    let (lines, board) = output.split_once("@@board").unwrap_or((output, ""));
    let mut probe = Probe {
        user: None,
        root: Err("the probe did not run".into()),
        tilo_vmd: false,
        bootc: false,
        arch: None,
        board: None,
    };
    for line in lines.lines().map(str::trim) {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "user" => probe.user = Some(value.to_owned()).filter(|v| !v.is_empty()),
            "sudo" => {
                probe.root = match value {
                    "root" => Ok(true),
                    "ok" => Ok(false),
                    other => Err(other
                        .strip_prefix("no")
                        .map(str::trim)
                        .and_then(|why| why.strip_prefix("sudo:"))
                        .map_or_else(|| other.to_owned(), |why| why.trim().to_owned())),
                };
            }
            "tilo-vmd" => probe.tilo_vmd = value == "yes",
            "bootc" => probe.bootc = value == "yes",
            "arch" => probe.arch = Some(value.to_owned()).filter(|v| !v.is_empty()),
            _ => {}
        }
    }
    probe.board = serde_json::from_str::<serde_json::Value>(board.trim())
        .ok()
        .and_then(|profile| profile.get("id")?.as_str().map(str::to_owned))
        .filter(|id| !id.is_empty());
    probe
}

/// The remote line `--grant-sudo` runs: a sudoers drop-in that lets `user`
/// run commands as root without a password, checked by visudo before it is
/// moved in place (sudo skips the `.new` file meanwhile). sudo asks for the
/// user's password on the terminal.
///
/// # Errors
/// A user name sudoers would not take literally.
pub fn grant_sudo_line(user: &str) -> Result<String, CliError> {
    let valid = !user.is_empty()
        && user.len() <= 32
        && user
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b == b'_')
        && user
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b));
    if !valid {
        return Err(CliError::InvalidInput(format!(
            "'{user}' is not a user name tilo-vm writes into sudoers; grant it by hand (tilo-vm guide devices)"
        )));
    }
    let file = sudoers_file(user);
    let inner = format!(
        "printf \"%s\\n\" \"# tilo-vm vm add-device --grant-sudo: tilo-vm runs tilo-vmd and its commands as root\" \"{user} ALL=(ALL) NOPASSWD: ALL\" > {file}.new && chmod 0440 {file}.new && visudo -cqf {file}.new && mv {file}.new {file}"
    );
    Ok(format!("sudo -- sh -c {}", sh_quote(&inner)))
}

/// The drop-in [`grant_sudo_line`] writes; removing it revokes the grant.
pub fn sudoers_file(user: &str) -> String {
    format!("/etc/sudoers.d/tilo-vm-{user}")
}

/// The checks behind `add-device`. `hello` is tilo-vmd's handshake, when
/// it got that far. The first three are required: without them no command
/// can work.
pub fn device_checks(
    endpoint: &SshEndpoint,
    probe: &Probe,
    hello: Option<&Result<HelloResult, CliError>>,
) -> Vec<Check> {
    let mut checks = vec![Check::ok(
        "ssh",
        format!(
            "{} answers as {}",
            endpoint.ssh_spec(),
            probe.user.as_deref().unwrap_or(&endpoint.user)
        ),
    )];
    checks.push(match &probe.root {
        Ok(true) => Check::ok("root", "logged in as root"),
        Ok(false) => Check::ok(
            "root",
            format!("{} runs commands as root with sudo -n", endpoint.user),
        ),
        Err(why) => Check::fail(
            "root",
            format!(
                "{} cannot run commands as root without a password ({why}); tilo-vm runs tilo-vmd and its own commands as root",
                endpoint.user
            ),
            format!(
                "rerun with --grant-sudo: sudo asks for {}'s password once, here, and {} lets it run commands as root without one   |   or log in as root: --ssh root@{}",
                endpoint.user,
                sudoers_file(&endpoint.user),
                endpoint.host
            ),
        ),
    });
    let image = "install a Tilo dev image on it (tilo-vm guide devices: a board dev image, or the dev disk)";
    checks.push(match (probe.tilo_vmd, hello) {
        (false, _) => Check::fail("tilo-vmd", "not installed: this is not a Tilo dev image", image),
        (true, None) => Check::fail(
            "tilo-vmd",
            "installed, not asked: it runs as root",
            "fix the root check above, then rerun",
        ),
        (true, Some(Ok(hello))) => Check::ok(
            "tilo-vmd",
            format!(
                "{} speaks protocol {} on {}",
                hello.daemon_version, hello.protocol, hello.arch
            ),
        ),
        // An older image: register anyway, so vm upgrade can move it on.
        (
            true,
            Some(Err(error @ CliError::ProtocolMismatch {
                vm, client, daemon, ..
            })),
        ) if daemon < client => Check::warn(
            "tilo-vmd",
            error.to_string(),
            format!(
                "slots and swaps wait for a newer image: tilo-vm vm upgrade {vm} (it needs only SSH and bootc)"
            ),
        ),
        (true, Some(Err(error))) => Check::fail("tilo-vmd", error.to_string(), error.hint()),
    });
    if let Some(Ok(hello)) = hello {
        // A device takes CI layers from this host, never pulling the image itself.
        let tools = describe_tools(&hello.tools, true);
        checks.push(if hello.tools.can_build() {
            Check::ok("toolchain", tools)
        } else {
            Check::warn(
                "toolchain",
                tools,
                "swaps from this worktree build on the device and need cargo, rsync and python3: move it onto a current board dev image (tilo-vm vm upgrade), or swap CI builds (--from sha:<commit>)",
            )
        });
        for issue in &hello.issues {
            checks.push(Check::warn(
                "slots",
                format!("slot {}: {}", issue.slot, issue.problem),
                issue.hint.clone(),
            ));
        }
    }
    checks.push(if probe.bootc {
        Check::ok(
            "bootc",
            match &probe.board {
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

/// Did the required checks pass?
pub fn usable(checks: &[Check]) -> bool {
    checks
        .iter()
        .filter(|c| ["ssh", "root", "tilo-vmd"].contains(&c.check.as_str()))
        .all(|c| c.state != "fail")
}

impl App {
    /// `vm add` for a device reached as `endpoint.user`, not root.
    #[expect(
        clippy::too_many_lines,
        reason = "one registration sequence: probe, checks, net facts, record"
    )]
    pub(super) fn vm_add_account(
        &mut self,
        args: &AddArgs,
        endpoint: &SshEndpoint,
    ) -> Result<Outcome, CliError> {
        let endpoint = endpoint.clone();
        let key = match &args.key {
            Some(key) if !key.is_file() => {
                return Err(CliError::InvalidInput(format!(
                    "--key {}: no such file",
                    key.display()
                )));
            }
            Some(key) => Some(absolute(key).display().to_string()),
            None => None,
        };
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
        if args.grant_sudo {
            self.grant_sudo(&endpoint, key.as_deref())?;
        }
        let probe_line = probe_command(&endpoint, key.as_deref());
        let output = self.runner.output(&probe_line)?;
        if !output.stdout.contains("user=") {
            return Err(CliError::Unreachable {
                vm: args.name.clone(),
                detail: if output.stderr.trim().is_empty() {
                    format!("`{}` printed nothing", probe_line.display())
                } else {
                    output.stderr.trim().to_owned()
                },
                debug: vec![format!(
                    "ssh {} true   (key-only login: BatchMode, no password)",
                    endpoint.ssh_spec()
                )],
            });
        }
        let probe = parse_probe(&output.stdout);
        let hello = (probe.root.is_ok() && probe.tilo_vmd).then(|| {
            Remote::connect(&args.name, &endpoint, key.as_deref(), None, Vec::new())
                .map(|remote| remote.hello.clone())
        });
        let checks = device_checks(&endpoint, &probe, hello.as_ref());
        let ok = usable(&checks);
        let mut text = checks_text(&args.name, &endpoint, &checks);
        let arch = match &hello {
            Some(Ok(hello)) => Some(hello.arch.clone()),
            _ => probe.arch.clone(),
        };
        let mut entry = VmEntry {
            hypervisor: DEVICE.into(),
            ssh: Some(endpoint.ssh_spec()),
            key,
            arch: arch.clone(),
            ..VmEntry::default()
        };
        if ok {
            self.add_net_facts(&endpoint, &mut entry, &mut text);
        }
        let data = json!({
            "vm": args.name,
            "ssh": endpoint.ssh_spec(),
            "board": probe.board,
            "arch": arch,
            "registered": ok,
            "checks": checks,
            "hostname": entry.hostname,
            "mac": entry.mac,
            "wake": entry.wake,
        });
        if !ok {
            text.push_str("\nnot registered: fix the failed checks above, then rerun this command");
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
        let use_it = format!("tilo-vm vm use {}", args.name);
        Ok(Outcome::new(text, data).next(&[
            use_it.as_str(),
            "tilo-vm slot create --headless",
            "tilo-vm guide devices",
        ]))
    }
}

impl App {
    /// Its name, MAC and wake setting, so the device is found again when
    /// its address changes and can be woken (tilo-vm vm wake).
    fn add_net_facts(&mut self, endpoint: &SshEndpoint, entry: &mut VmEntry, text: &mut String) {
        let Some(facts) = self.net_facts(endpoint, entry.key.as_deref()) else {
            return;
        };
        super::reach::apply_net(entry, &facts);
        if let Some(wake) = &entry.wake {
            let _ = write!(text, "\n  wake: {}", wake.describe());
        }
    }

    /// `--grant-sudo`: write the sudoers drop-in over an interactive SSH
    /// session, where sudo asks for the user's password.
    fn grant_sudo(&mut self, endpoint: &SshEndpoint, key: Option<&str>) -> Result<(), CliError> {
        let mut line = remote::ssh_transport(endpoint, key, true);
        line.args.push(grant_sudo_line(&endpoint.user)?);
        if !self.json {
            eprintln!(
                "tilo-vm: letting {} run commands as root without a password ({}); sudo asks for its password",
                endpoint.user,
                sudoers_file(&endpoint.user)
            );
        }
        if self.runner.interactive(&line)? {
            Ok(())
        } else {
            Err(CliError::CommandFailed {
                command: line.display(),
                status: "sudo did not write the grant".into(),
                stderr: "its output above says why".into(),
            })
        }
    }
}

/// The checks as `vm add` prints them, each failure with its fix.
fn checks_text(name: &str, endpoint: &SshEndpoint, checks: &[Check]) -> String {
    let mut text = format!("device {name} ({})", endpoint.ssh_spec());
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

/// `ssh ... <user>@<host> sh -c <probe>`, as the login user.
pub fn probe_command(endpoint: &SshEndpoint, key: Option<&str>) -> CommandLine {
    let mut line = remote::ssh_transport(endpoint, key, false);
    line.args.push(format!("sh -c {}", sh_quote(PROBE)));
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(user: &str) -> SshEndpoint {
        SshEndpoint {
            user: user.into(),
            host: "10.0.0.223".into(),
            port: 22,
        }
    }

    fn hello() -> HelloResult {
        serde_json::from_value(json!({
            "protocol": tilo_vm_protocol::PROTOCOL_VERSION,
            "daemon_version": "0.1.0",
            "root": "/var/lib/tilo-vm",
            "arch": "aarch64",
            "issues": [],
            "tools": {"cargo": true, "rsync": true, "python3": true, "sccache": true, "podman": true}
        }))
        .unwrap()
    }

    /// The probe reads who answered, whether root is reachable without a password, and what the
    /// image carries; the checks require SSH, root and tilo-vmd, and name each fix.
    #[test]
    fn add_device_checks_root_daemon_and_toolchain_and_names_each_fix() {
        // Phase: the_probe_parses_a_board_device.
        let board = r#"{"schema_version": 1, "id": "lenovo-83ed", "architecture": "aarch64"}"#;
        let output = format!(
            "motd line\nuser=me\nsudo=no sudo: a password is required\ntilo-vmd=yes\nbootc=yes\narch=aarch64\n@@board\n{board}\n"
        );
        let probe = parse_probe(&output);
        assert_eq!(probe.user.as_deref(), Some("me"));
        assert_eq!(probe.root, Err("a password is required".into()));
        assert!(probe.tilo_vmd && probe.bootc);
        assert_eq!(probe.arch.as_deref(), Some("aarch64"));
        assert_eq!(probe.board.as_deref(), Some("lenovo-83ed"));
        assert_eq!(
            parse_probe("user=root\nsudo=root\ntilo-vmd=no\nbootc=no\narch=x86_64\n@@board\n").root,
            Ok(true)
        );
        assert_eq!(parse_probe("user=me\nsudo=ok\n@@board\n").board, None);

        // Phase: without_root_it_is_not_registered_and_says_how_to_grant_it.
        let checks = device_checks(&endpoint("me"), &probe, None);
        assert!(!usable(&checks));
        let root = checks.iter().find(|c| c.check == "root").unwrap();
        assert_eq!(root.state, "fail");
        let fix = root.fix.as_deref().unwrap();
        assert!(
            fix.contains("--grant-sudo") && fix.contains("/etc/sudoers.d/tilo-vm-me"),
            "{fix}"
        );
        assert!(fix.contains("--ssh root@10.0.0.223"), "{fix}");
        let daemon = checks.iter().find(|c| c.check == "tilo-vmd").unwrap();
        assert_eq!(daemon.state, "fail");

        // Phase: with_sudo_and_a_daemon_it_is_usable.
        let probe = Probe {
            root: Ok(false),
            ..probe
        };
        let answered = Ok(hello());
        let checks = device_checks(&endpoint("me"), &probe, Some(&answered));
        assert!(usable(&checks), "{checks:?}");
        let bootc = checks.iter().find(|c| c.check == "bootc").unwrap();
        assert!(
            bootc.detail.contains("lenovo-83ed board image"),
            "{}",
            bootc.detail
        );
        assert_eq!(
            checks
                .iter()
                .find(|c| c.check == "toolchain")
                .unwrap()
                .state,
            "ok"
        );

        // Phase: a_missing_toolchain_or_daemon_is_named.
        let mut thin = hello();
        thin.tools.cargo = false;
        let checks = device_checks(&endpoint("me"), &probe, Some(&Ok(thin)));
        assert!(usable(&checks));
        let toolchain = checks.iter().find(|c| c.check == "toolchain").unwrap();
        assert_eq!(toolchain.state, "warn");
        let no_daemon = Probe {
            tilo_vmd: false,
            ..probe.clone()
        };
        let checks = device_checks(&endpoint("me"), &no_daemon, None);
        assert!(!usable(&checks));
        let refused = Err(CliError::ProtocolMismatch {
            vm: "yoga".into(),
            client: 6,
            daemon: 5,
            bootc: Some(true),
        });
        // An older daemon still registers, so vm upgrade can bring a newer one.
        let checks = device_checks(&endpoint("me"), &probe, Some(&refused));
        assert!(usable(&checks));
        let daemon = checks.iter().find(|c| c.check == "tilo-vmd").unwrap();
        assert_eq!(daemon.state, "warn");
        assert!(
            daemon
                .fix
                .as_deref()
                .unwrap()
                .contains("tilo-vm vm upgrade yoga ")
        );
        let newer = Err(CliError::ProtocolMismatch {
            vm: "yoga".into(),
            client: 5,
            daemon: 6,
            bootc: Some(true),
        });
        assert!(!usable(&device_checks(
            &endpoint("me"),
            &probe,
            Some(&newer)
        )));
    }

    /// The sudo grant is one validated drop-in, checked by visudo before it is used; odd names
    /// are refused rather than written into sudoers.
    #[test]
    fn granting_sudo_writes_one_checked_drop_in() {
        let line = grant_sudo_line("xaver").unwrap();
        assert!(line.starts_with("sudo -- sh -c '"), "{line}");
        assert!(line.contains("xaver ALL=(ALL) NOPASSWD: ALL"), "{line}");
        assert!(
            line.contains("visudo -cqf /etc/sudoers.d/tilo-vm-xaver.new && mv /etc/sudoers.d/tilo-vm-xaver.new /etc/sudoers.d/tilo-vm-xaver"),
            "{line}"
        );
        // No single quote inside: the line survives any login shell's quoting.
        assert_eq!(line.matches('\'').count(), 2, "{line}");
        for bad in ["", "Root", "a b", "x;rm", "a'b", &"x".repeat(33)] {
            assert!(grant_sudo_line(bad).is_err(), "{bad}");
        }
        let probe = probe_command(&endpoint("me"), Some("/k/yoga"));
        let last = probe.args.last().unwrap();
        assert!(last.starts_with("sh -c '"), "{last}");
        assert!(
            !probe.args.iter().any(|a| a == "sudo"),
            "the probe runs as the login user"
        );
    }
}
