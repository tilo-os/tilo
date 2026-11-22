//! The per-VM root SSH key. `vm create` makes an ed25519 key per VM in `keys/`
//! beside `vms.toml` (kept when it already exists); the dev image reads the
//! public key from the systemd credential [`CREDENTIAL`] and writes it to
//! `/root/.ssh/authorized_keys` at boot.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use crate::error::CliError;
use crate::hypervisor::{CommandLine, Runner, SshEndpoint, run_all};

pub const CREDENTIAL: &str = "ssh.authorized_keys.root";

/// The dev image's password login (`os/account.env`), the way in before root
/// has a key.
pub const DEV_ACCOUNT: &str = "tester";

pub fn key_path(registry_path: &Path, name: &str) -> PathBuf {
    registry_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("keys")
        .join(format!("{name}_ed25519"))
}

pub fn public_path(key: &Path) -> PathBuf {
    let mut name = key.as_os_str().to_owned();
    name.push(".pub");
    PathBuf::from(name)
}

pub fn keygen_command(key: &Path, name: &str) -> CommandLine {
    CommandLine::new("ssh-keygen", &["-q", "-t", "ed25519", "-N", ""])
        .arg("-C")
        .arg(format!("tilo-vm-{name}"))
        .arg("-f")
        .arg(key.display().to_string())
}

pub fn read_public(key: &Path) -> Result<String, CliError> {
    let path = public_path(key);
    let text =
        std::fs::read_to_string(&path).map_err(CliError::io(format!("read {}", path.display())))?;
    let line = text.lines().next().unwrap_or_default().trim().to_owned();
    if line.is_empty() {
        return Err(CliError::io(format!("read {}", path.display()))(
            std::io::Error::other("empty public key"),
        ));
    }
    Ok(line)
}

pub fn ensure(runner: &mut dyn Runner, key: &Path, name: &str) -> Result<String, CliError> {
    if !key.exists() {
        if let Some(dir) = key.parent() {
            create_private_dir(dir)?;
        }
        run_all(runner, &[keygen_command(key, name)])?;
    }
    read_public(key)
}

fn create_private_dir(dir: &Path) -> Result<(), CliError> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
        .create(dir)
        .map_err(CliError::io(format!("create {}", dir.display())))
}

/// The public key as `authorized_keys` content, base64-encoded: no spaces,
/// quotes or commas, so it survives every command line it passes through.
pub fn encoded(public: &str) -> String {
    STANDARD.encode(format!("{public}\n"))
}

/// The SMBIOS type 11 OEM string systemd imports as [`CREDENTIAL`].
pub fn smbios_credential(public: &str) -> String {
    format!(
        "io.systemd.credential.binary:{CREDENTIAL}={}",
        encoded(public)
    )
}

/// Direct Linux boots on QEMU's ARM `virt` machine have no SMBIOS table for
/// systemd to read. The public key can safely use systemd's kernel command
/// line credential transport; private keys and passwords must never use it.
pub fn kernel_credential(public: &str) -> String {
    format!(
        "systemd.set_credential_binary={CREDENTIAL}:{}",
        encoded(public)
    )
}

/// The VM's own `known_hosts`, beside its key. Local VMs all answer on
/// 127.0.0.1 with forwarded ports, and a recreated VM has new host keys, so
/// one shared `~/.ssh/known_hosts` entry per port would refuse the next VM.
pub fn known_hosts_path(key: &str) -> String {
    format!("{key}.known_hosts")
}

pub fn ssh_args(endpoint: &SshEndpoint, key: Option<&str>, interactive: bool) -> Vec<String> {
    let mut args: Vec<String> = [
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "StrictHostKeyChecking=accept-new",
        "-o",
        "ServerAliveInterval=15",
    ]
    .map(str::to_owned)
    .to_vec();
    if interactive {
        args.push("-t".into());
    }
    if let Some(key) = key {
        args.extend([
            "-o".into(),
            format!("UserKnownHostsFile={}", known_hosts_path(key)),
            "-i".into(),
            key.to_owned(),
        ]);
    }
    args.extend([
        "-p".into(),
        endpoint.port.to_string(),
        format!("{}@{}", endpoint.user, endpoint.host),
    ]);
    args
}

/// `vm ssh`'s command line: the shell people open on a VM, as root. Nothing else tilo-vm
/// does goes over SSH.
pub fn ssh_command(
    endpoint: &SshEndpoint,
    key: Option<&str>,
    remote: &[String],
    interactive: bool,
) -> CommandLine {
    let mut command = ssh_transport(endpoint, key, interactive);
    command.args.extend(as_root(endpoint, remote));
    command
}

pub fn ssh_transport(endpoint: &SshEndpoint, key: Option<&str>, interactive: bool) -> CommandLine {
    let program = std::env::var("TILO_VM_SSH").unwrap_or_else(|_| "ssh".into());
    let mut command = CommandLine::new(&program, &[]);
    command.args = ssh_args(endpoint, key, interactive);
    command
}

/// A device usually lets only its owner's account in over SSH, so a shell there goes through
/// `sudo`; ssh hands the remote words to the login shell joined by spaces, so the line runs as
/// `sh -c` under sudo, and with no command it is a root login shell.
pub fn as_root(endpoint: &SshEndpoint, remote: &[String]) -> Vec<String> {
    if endpoint.user == "root" {
        return remote.to_vec();
    }
    if remote.is_empty() {
        return vec!["sudo".into(), "-i".into()];
    }
    vec![
        "sudo".into(),
        "-n".into(),
        "--".into(),
        "sh".into(),
        "-c".into(),
        crate::vmbuild::sh_quote(&remote.join(" ")),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const PUBLIC: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample tilo-vm-dev";

    #[test]
    fn the_dev_ssh_key_lives_beside_the_registry_and_logs_in_as_dev() {
        #[cfg(unix)]
        {
            let key = key_path(Path::new("/cfg/tilo-vm/vms.toml"), "dev");
            assert_eq!(key, Path::new("/cfg/tilo-vm/keys/dev_ed25519"));
            assert_eq!(
                public_path(&key),
                Path::new("/cfg/tilo-vm/keys/dev_ed25519.pub")
            );
            assert_eq!(
                keygen_command(&key, "dev").display(),
                "ssh-keygen -q -t ed25519 -N '' -C tilo-vm-dev -f /cfg/tilo-vm/keys/dev_ed25519"
            );
        }

        {
            let value = smbios_credential(PUBLIC);
            let encoded = value
                .strip_prefix("io.systemd.credential.binary:ssh.authorized_keys.root=")
                .unwrap();
            assert!(!encoded.contains([' ', ',', '\'', '"']), "{encoded}");
            assert_eq!(
                STANDARD.decode(encoded).unwrap(),
                format!("{PUBLIC}\n").as_bytes()
            );
            let kernel = kernel_credential(PUBLIC);
            let encoded = kernel
                .strip_prefix("systemd.set_credential_binary=ssh.authorized_keys.root:")
                .unwrap();
            assert_eq!(
                STANDARD.decode(encoded).unwrap(),
                format!("{PUBLIC}\n").as_bytes()
            );
        }

        {
            struct Record(Vec<String>);
            impl Runner for Record {
                fn output(
                    &mut self,
                    command: &CommandLine,
                ) -> Result<crate::hypervisor::Output, CliError> {
                    self.0.push(command.display());
                    Err(CliError::InvalidInput("not run".into()))
                }
                fn interactive(&mut self, command: &CommandLine) -> Result<bool, CliError> {
                    self.0.push(command.display());
                    Ok(false)
                }
            }
            let dir = tempfile::tempdir().unwrap();
            let key = dir.path().join("keys/dev_ed25519");
            std::fs::create_dir_all(key.parent().unwrap()).unwrap();
            std::fs::write(&key, "private").unwrap();
            std::fs::write(public_path(&key), format!("{PUBLIC}\n")).unwrap();
            let mut runner = Record(Vec::new());
            assert_eq!(ensure(&mut runner, &key, "dev").unwrap(), PUBLIC);
            assert!(runner.0.is_empty(), "{:?}", runner.0);
            let other = dir.path().join("keys/other_ed25519");
            assert!(ensure(&mut runner, &other, "other").is_err());
            assert_eq!(runner.0, [keygen_command(&other, "other").display()]);
        }
    }
}
