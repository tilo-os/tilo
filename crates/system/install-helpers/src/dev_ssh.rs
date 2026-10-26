//! `tilo-install-dev-ssh --etc ETC --username USERNAME --key KEY`: applies
//! explicit owner SSH keys to a self-installed development image.
//!
//! The installer calls this only for a dev self-install with a setup handoff.
//! It does not create a personal identity or change onboarding state: it
//! writes the account's `authorized_keys` file, a key-only `sshd` drop-in and
//! the `sshd.service` enablement link, each labelled for its installed path.

use std::ffi::OsStr;
use std::fs::{self, DirBuilder, OpenOptions, Permissions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::args::{Arg, Command};
use crate::{Error, Result, py, require};

/// How long one installed-system tool may take before it is treated as
/// wedged. `ssh-keygen` over one bounded key file and `chcon` over one path
/// are immediate; a minute is already generous.
const TOOL_TIMEOUT: Duration = Duration::from_secs(60);

/// The command line.
pub const COMMAND: Command = Command {
    prog: "tilo-install-dev-ssh",
    usage: "usage: tilo-install-dev-ssh [-h] --etc ETC --username USERNAME --key KEY\n",
    help: "
Apply explicit owner SSH keys to a self-installed development image.

The installer calls this only for a dev self-install with a setup handoff. It
does not create a personal identity or change onboarding state.

options:
  -h, --help           show this help message and exit
  --etc ETC
  --username USERNAME
  --key KEY
",
    args: &[
        Arg::Option {
            flag: "--etc",
            required: true,
        },
        Arg::Option {
            flag: "--username",
            required: true,
        },
        Arg::Option {
            flag: "--key",
            required: true,
        },
    ],
};

/// At most this many keys in one request.
pub const MAX_KEYS: usize = 16;

const KEY_TYPES: [&str; 5] = [
    "ssh-ed25519",
    "ssh-rsa",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
];

const BRINGUP_CONFIG: &str = "PermitRootLogin no\nPasswordAuthentication no\n\
KbdInteractiveAuthentication no\nPubkeyAuthentication yes\n\
AuthorizedKeysFile /etc/ssh/authorized_keys/%u\n";

const SSHD_UNIT: &str = "/usr/lib/systemd/system/sshd.service";

/// The installed system's tools. The provided methods are the production
/// behaviour; tests replace them.
pub trait Host {
    /// Labels `path` with the security context policy gives `installed`.
    ///
    /// # Errors
    /// `matchpathcon` or `chcon` failing.
    fn label(&self, path: &Path, installed: &str) -> Result<()> {
        let output = crate::process::run_os(
            &[
                OsStr::new("/usr/sbin/matchpathcon"),
                OsStr::new("-n"),
                OsStr::new(installed),
            ],
            None,
            TOOL_TIMEOUT,
        )?;
        require(output.success(), "matchpathcon failed")?;
        let context = String::from_utf8_lossy(&output.stdout);
        let status = crate::process::run_os(
            &[
                OsStr::new("chcon"),
                OsStr::new("--no-dereference"),
                OsStr::new(context.trim()),
                path.as_os_str(),
            ],
            None,
            TOOL_TIMEOUT,
        )?;
        require(status.success(), "chcon failed")
    }

    /// `ssh-keygen` reads every key in `path`.
    ///
    /// # Errors
    /// `ssh-keygen` refusing the file.
    fn check_keys(&self, path: &Path) -> Result<()> {
        let output = crate::process::run_os(
            &[
                OsStr::new("ssh-keygen"),
                OsStr::new("-l"),
                OsStr::new("-f"),
                path.as_os_str(),
            ],
            None,
            TOOL_TIMEOUT,
        )?;
        require(output.success(), "ssh-keygen refused the SSH public keys")
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct System;

impl Host for System {}

/// `[a-z_][a-z0-9_-]{0,31}`.
fn valid_username(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_lowercase() || first == b'_')
        && name.len() <= 32
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// The request's keys, one OpenSSH public key per line, normalized to
/// `TYPE BASE64\n` each.
///
/// # Errors
/// A linked, oversized or non-ASCII file; options before a key; an
/// unsupported or mislabelled key; more than [`MAX_KEYS`] or none.
pub fn public_keys(host: &dyn Host, path: &Path) -> Result<String> {
    let info = fs::symlink_metadata(path)?;
    require(
        path.is_file()
            && !info.file_type().is_symlink()
            && info.nlink() == 1
            && info.size() <= 32768,
        "SSH key must be a bounded, non-linked regular file",
    )?;
    let bytes = fs::read(path)?;
    require(bytes.is_ascii(), "SSH public keys must be ASCII")?;
    let text = py::universal_newlines(&String::from_utf8_lossy(&bytes));
    let lines: Vec<&str> = text
        .split('\n')
        .map(py::strip)
        .filter(|line| !line.is_empty())
        .collect();
    require(
        !lines.is_empty() && lines.len() <= MAX_KEYS,
        &format!("expected 1 to {MAX_KEYS} SSH public keys"),
    )?;
    let mut keys = String::new();
    for line in lines {
        let parts = py::split(line);
        let (Some(kind), Some(data)) = (parts.first(), parts.get(1)) else {
            return Err(Error::new(
                "expected SSH public keys without authorized_keys options",
            ));
        };
        require(KEY_TYPES.contains(kind), "unsupported SSH public key type")?;
        let blob = py::b64decode(data).map_err(|error| Error::new(error.to_string()))?;
        // A blob shorter than its length prefix has no type to match.
        let length = blob
            .get(..4)
            .and_then(|prefix| <[u8; 4]>::try_from(prefix).ok())
            .map_or(0, u32::from_be_bytes);
        let start = blob.len().min(4);
        let end = blob.len().min(
            usize::try_from(length)
                .unwrap_or(usize::MAX)
                .saturating_add(4),
        );
        require(
            blob.get(start..end) == Some(kind.as_bytes()),
            "SSH public key type differs from its wire encoding",
        )?;
        keys.push_str(kind);
        keys.push(' ');
        keys.push_str(data);
        keys.push('\n');
    }
    host.check_keys(path)?;
    Ok(keys)
}

/// `root/relative`, creating each missing directory 0755 and refusing a link.
fn directory(root: &Path, relative: &str) -> Result<PathBuf> {
    let mut current = root.to_path_buf();
    for part in relative.split('/') {
        current.push(part);
        require(
            !current.is_symlink(),
            "installed SSH path contains a symlink",
        )?;
        if let Err(error) = DirBuilder::new().mode(0o755).create(&current)
            && !(error.kind() == std::io::ErrorKind::AlreadyExists && current.is_dir())
        {
            return Err(error.into());
        }
        require(current.is_dir(), "installed SSH path is not a directory")?;
    }
    Ok(current)
}

fn installed(etc: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(etc).unwrap_or(path);
    format!("/etc/{}", relative.display())
}

fn present(path: &Path) -> bool {
    path.exists() || path.is_symlink()
}

/// Writes the owner keys, the key-only drop-in and the enablement link under
/// the installed `etc` for `username`.
///
/// # Errors
/// An invalid `/etc` or account, refused keys, a link in the way, an output
/// that already exists, or a tool failing.
pub fn apply(host: &dyn Host, etc: &Path, username: &str, key_path: &Path) -> Result<()> {
    require(
        !etc.is_symlink() && etc.is_dir() && valid_username(username),
        "invalid installed /etc or account",
    )?;
    let key = public_keys(host, key_path)?;
    let keys = directory(etc, "ssh/authorized_keys")?;
    let config = directory(etc, "ssh/sshd_config.d")?;
    let wants = directory(etc, "systemd/system/multi-user.target.wants")?;
    for path in [&keys, &config, &wants] {
        host.label(path, &installed(etc, path))?;
    }
    let files = [
        (keys.join(username), key),
        (
            config.join("00-tilo-bringup.conf"),
            format!("AllowUsers {username}\n{BRINGUP_CONFIG}"),
        ),
    ];
    // These are request-specific outputs. Refuse unrelated or linked state.
    for (path, _) in &files {
        require(!present(path), "SSH provisioning output already exists")?;
    }
    let enable = wants.join("sshd.service");
    require(
        !present(&enable),
        "SSH enablement must first be cleared by the installer",
    )?;
    for (path, content) in &files {
        let mut stream = OpenOptions::new().write(true).create_new(true).open(path)?;
        fs::set_permissions(path, Permissions::from_mode(0o644))?;
        stream.write_all(content.as_bytes())?;
        stream.flush()?;
        stream.sync_all()?;
        host.label(path, &installed(etc, path))?;
    }
    std::os::unix::fs::symlink(SSHD_UNIT, &enable)?;
    host.label(
        &enable,
        "/etc/systemd/system/multi-user.target.wants/sshd.service",
    )
}

/// The command: `apply` on the production host.
///
/// # Errors
/// As [`apply`].
pub fn run(values: &crate::args::Values) -> Result<()> {
    let username = values.required("--username")?;
    apply(
        &System,
        Path::new(values.required("--etc")?),
        &username.to_string_lossy(),
        Path::new(values.required("--key")?),
    )
}
