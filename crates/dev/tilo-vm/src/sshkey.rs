//! The per-VM root SSH key.
//!
//! `vm create` makes an ed25519 key per VM in `keys/` beside `vms.toml`
//! (kept when it already exists) and records its path as the VM's `key`;
//! every SSH connection to the VM uses it. The dev image reads the public
//! key from the systemd credential [`CREDENTIAL`], which systemd's provision
//! tmpfiles write to `/root/.ssh/authorized_keys` at boot (see
//! `os/dev-image/etc/ssh/sshd_config.d/40-tilo-vm.conf`). Adapters with a
//! credential channel pass it at creation ([`smbios_credential`]); the rest
//! need [`install_command`] once after the first boot.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use crate::error::CliError;
use crate::hypervisor::{CommandLine, Runner, SshEndpoint, run_all};

/// The credential the dev image turns into root's `authorized_keys`.
pub const CREDENTIAL: &str = "ssh.authorized_keys.root";

/// The dev image's password login (`os/account.env`), the way in before root
/// has a key.
pub const DEV_ACCOUNT: &str = "tester";

/// Where VM `name`'s private key lives; the public key is beside it as `.pub`.
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

/// `ssh-keygen` for a new, passphrase-less key at `key`.
pub fn keygen_command(key: &Path, name: &str) -> CommandLine {
    CommandLine::new("ssh-keygen", &["-q", "-t", "ed25519", "-N", ""])
        .arg("-C")
        .arg(format!("tilo-vm-{name}"))
        .arg("-f")
        .arg(key.display().to_string())
}

/// The public key line of `key`.
///
/// # Errors
/// The `.pub` file is missing or empty.
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

/// Make VM `name`'s key at `key` unless it exists; returns its public key.
///
/// # Errors
/// `ssh-keygen` is missing or fails, or the key directory cannot be made.
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

/// The one-time step for a VM that did not get the key as a credential:
/// the dev account logs in with its password, and sudo appends the key to
/// root's `authorized_keys`. Runs the same in a POSIX shell and `PowerShell`.
pub fn install_command(endpoint: &SshEndpoint, public: &str) -> String {
    format!(
        "ssh -t -o StrictHostKeyChecking=accept-new -p {} {DEV_ACCOUNT}@{} 'sudo install -d -m 0700 /root/.ssh && echo {} | base64 -d | sudo tee -a /root/.ssh/authorized_keys >/dev/null && sudo restorecon -R /root/.ssh'",
        endpoint.port,
        endpoint.host,
        encoded(public)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const PUBLIC: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIExample tilo-vm-dev";

    /// Keys live beside the registry, the credential carries the key line base64-encoded, the install step
    /// logs in as the dev account, and an existing key is kept.
    #[test]
    fn the_dev_ssh_key_lives_beside_the_registry_and_logs_in_as_dev() {
        // Phase: keys_live_beside_the_registry.
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

        // Phase: the_credential_carries_the_key_line_base64_encoded.
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

        // Phase: the_install_step_logs_in_as_the_dev_account.
        {
            let endpoint = SshEndpoint {
                user: "root".into(),
                host: "192.168.64.5".into(),
                port: 22,
            };
            let command = install_command(&endpoint, PUBLIC);
            assert!(
                command.starts_with("ssh -t -o StrictHostKeyChecking=accept-new -p 22 tester@192.168.64.5 'sudo install -d -m 0700 /root/.ssh && echo "),
                "{command}"
            );
            assert!(command.contains(&encoded(PUBLIC)));
            assert!(command.ends_with("| base64 -d | sudo tee -a /root/.ssh/authorized_keys >/dev/null && sudo restorecon -R /root/.ssh'"));
        }

        // Phase: an_existing_key_is_kept.
        {
            /// Records what would have run, and runs nothing.
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
            // A new VM's key is made with ssh-keygen.
            let other = dir.path().join("keys/other_ed25519");
            assert!(ensure(&mut runner, &other, "other").is_err());
            assert_eq!(runner.0, [keygen_command(&other, "other").display()]);
        }
    }
}
