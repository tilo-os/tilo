//! The in-VM builder's host half: sync the worktree into the VM with rsync
//! over the same SSH transport, then run a streaming `tilo-vmd` job there
//! (`build` or `ci-layer`) whose output shows live on this terminal.

use std::io::{Read as _, Write as _};
use std::path::Path;
use std::process::{Command, Stdio};

use sha2::{Digest, Sha256};
use tilo_vm_protocol::{JobLine, JobOutput, SRC_DIR};

use crate::context::{Worktree, slot_name_from};
use crate::error::CliError;
use crate::hypervisor::{CommandLine, SshEndpoint};
use crate::remote::{self, DEFAULT_DAEMON};

/// The VM's directory name for this host's worktree: readable, and unique
/// per host and path so two machines' `main` checkouts never share a build dir.
pub fn worktree_id(worktree: &Worktree, host: Option<&str>) -> String {
    let name = worktree
        .root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .and_then(|n| slot_name_from(&n))
        .unwrap_or_else(|| "worktree".into());
    let digest = Sha256::digest(
        format!("{}\0{}", host.unwrap_or_default(), worktree.root.display()).as_bytes(),
    );
    let short: String = name.chars().take(40).collect();
    format!(
        "{}-{}",
        short.trim_end_matches('-'),
        hex::encode(digest.get(..4).unwrap_or_default())
    )
}

/// POSIX shell quoting for one word.
pub fn sh_quote(word: &str) -> String {
    if !word.is_empty()
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./=:@,+".contains(&b))
    {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

/// rsync on Windows is MSYS2, so its SSH child must come from the same
/// runtime. Native Windows OpenSSH loses the rsync pipe before any bytes are
/// sent (ERROR from cb :87). Its key and known-hosts paths must be MSYS paths.
///
/// # Errors
/// Returns an error when rsync or its matching MSYS2 SSH executable is absent.
pub fn rsync_rsh(endpoint: &SshEndpoint, key: Option<&str>) -> Result<String, CliError> {
    let _rsync = crate::host::rsync_program().ok_or_else(|| CliError::ToolMissing {
        tool: "rsync".into(),
        purpose: "sync worktree and image layers into a VM".into(),
    })?;
    let ssh = remote::ssh_command(endpoint, key, &[], false);
    #[cfg(windows)]
    let mut ssh = ssh;
    #[cfg(windows)]
    {
        let sibling = crate::host::rsync_ssh_program().ok_or_else(|| CliError::ToolMissing {
            tool: "MSYS2 OpenSSH (pacman -S openssh)".into(),
            purpose: format!("run as the SSH transport for {}", _rsync),
        })?;
        use_msys_ssh(&mut ssh, &sibling);
    }
    let mut words = vec![sh_quote(&ssh.program)];
    let options = ssh.args.len().saturating_sub(1);
    words.extend(ssh.args.iter().take(options).map(|arg| sh_quote(arg)));
    Ok(words.join(" "))
}

#[cfg(windows)]
fn use_msys_ssh(ssh: &mut CommandLine, sibling: &Path) {
    ssh.program = rsync_source(sibling);
    let mut key_next = false;
    for arg in &mut ssh.args {
        if key_next {
            *arg = rsync_source(Path::new(arg));
            key_next = false;
        } else if arg == "-i" {
            key_next = true;
        } else if let Some(path) = arg.strip_prefix("UserKnownHostsFile=") {
            *arg = format!("UserKnownHostsFile={}", rsync_source(Path::new(path)));
        }
    }
}

/// `rsync` that mirrors the worktree into `SRC_DIR/<id>/` in the VM:
/// `.gitignore` rules apply, `.git` and `target/` stay behind (the VM's
/// `target/` is its own build dir and is never deleted by the sync), and
/// the remote side holds the worktree's build lock while it writes.
///
/// # Errors
/// Returns an error when the rsync SSH transport is unavailable.
pub fn rsync_command(
    root: &Path,
    endpoint: &SshEndpoint,
    key: Option<&str>,
    id: &str,
) -> Result<CommandLine, CliError> {
    let rsh = rsync_rsh(endpoint, key)?;
    let dest_dir = format!("{SRC_DIR}/{id}");
    let locks = "/var/lib/tilo-vm/locks";
    let rsync_path =
        format!("mkdir -p {locks} {dest_dir} && flock --nonblock {locks}/src-{id}.lock rsync");
    let mut source = rsync_source(root);
    if !source.ends_with('/') {
        source.push('/');
    }
    Ok(CommandLine::new(
        &crate::host::rsync_program().unwrap_or_else(|| "rsync".into()),
        &[
            "--archive",
            "--delete",
            "--compress",
            "--filter=:- .gitignore",
            "--exclude=/target/",
            "--exclude=.git",
            "--exclude=/.tilo-vm",
        ],
    )
    .arg("--rsh")
    .arg(rsh)
    .arg("--rsync-path")
    .arg(rsync_path)
    .arg(source)
    .arg(format!("{}@{}:{dest_dir}/", endpoint.user, endpoint.host)))
}

/// MSYS2 rsync parses `C:\...` as a remote `host:path` operand. Use its
/// `/c/...` mount syntax for a Windows worktree passed as a local source.
pub(crate) fn rsync_source(root: &Path) -> String {
    let path = root.display().to_string();
    #[cfg(windows)]
    {
        let bytes = path.as_bytes();
        if let (Some(&drive), Some(&b':'), Some(suffix)) =
            (bytes.first(), bytes.get(1), path.get(2..))
            && drive.is_ascii_alphabetic()
        {
            let drive = (drive as char).to_ascii_lowercase();
            return format!("/{drive}{}", suffix.replace('\\', "/"));
        }
    }
    path
}

/// The remote command for a `tilo-vmd` subcommand, following the VM's
/// configured daemon command (e.g. `sudo -n tilo-vmd rpc`).
pub fn daemon_command(configured: Option<&str>, subcommand: &str) -> Vec<String> {
    let mut words: Vec<String> = configured
        .unwrap_or(DEFAULT_DAEMON)
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    if words.last().is_some_and(|w| w == "rpc") {
        words.pop();
    }
    words.push(subcommand.to_owned());
    words
}

/// Run a streaming job: `job` goes to the remote stdin, the remote stderr
/// streams to ours, and the job line comes back on stdout.
///
/// # Errors
/// ssh failures, a missing daemon, and a job that printed no result.
pub fn run_job(
    vm: &str,
    endpoint: &SshEndpoint,
    key: Option<&str>,
    remote_command: &[String],
    job: &impl serde::Serialize,
) -> Result<JobOutput, CliError> {
    let line = remote::ssh_command(endpoint, key, remote_command, false);
    let mut child = Command::new(&line.program)
        .args(&line.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                CliError::ToolMissing {
                    tool: line.program.clone(),
                    purpose: "reach the VM".into(),
                }
            } else {
                CliError::io("start ssh")(e)
            }
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        let bytes = serde_json::to_vec(job).map_err(|e| CliError::CommandFailed {
            command: remote_command.join(" "),
            status: "bad job".into(),
            stderr: e.to_string(),
        })?;
        stdin
            .write_all(&bytes)
            .map_err(CliError::io("send the job to the VM"))?;
    }
    let mut stdout = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        pipe.read_to_string(&mut stdout)
            .map_err(CliError::io("read the job result"))?;
    }
    let status = child.wait().map_err(CliError::io("wait for ssh"))?;
    match JobLine::find(&stdout) {
        Some(JobLine {
            result: Some(result),
            ..
        }) => Ok(result),
        Some(JobLine {
            error: Some(error), ..
        }) => Err(CliError::Remote(error)),
        _ if status.code() == Some(127) => Err(CliError::DaemonMissing {
            vm: vm.into(),
            bootc: None,
        }),
        _ if status.code() == Some(255) => Err(CliError::Unreachable {
            vm: vm.into(),
            detail: "ssh failed during the job".into(),
            debug: Vec::new(),
        }),
        _ => Err(CliError::CommandFailed {
            command: remote_command.join(" "),
            status: status.to_string(),
            stderr:
                "no result line; the output above says why (an old tilo-vmd? tilo-vm vm upgrade)"
                    .into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn endpoint() -> SshEndpoint {
        SshEndpoint {
            user: "root".into(),
            host: "127.0.0.1".into(),
            port: 2223,
        }
    }

    /// Worktree ids are readable and unique per host; rsync respects gitignore and leaves target alone; the
    /// Windows worktree is a local MSYS2 rsync operand; job commands follow the configured daemon.
    #[test]
    fn vm_builds_sync_each_worktree_to_its_configured_daemon() {
        // Phase: worktree_ids_are_readable_and_unique_per_host.
        {
            let wt = Worktree {
                root: PathBuf::from("/home/a/tilo/.claude/worktrees/agent-A74"),
                branch: None,
            };
            let a = worktree_id(&wt, Some("laptop"));
            assert!(a.starts_with("agent-a74-"), "{a}");
            assert_eq!(a.len(), "agent-a74-".len() + 8);
            assert_ne!(a, worktree_id(&wt, Some("desktop")));
            assert_eq!(a, worktree_id(&wt, Some("laptop")));
        }

        // Phase: rsync_respects_gitignore_and_leaves_target_alone.
        {
            let line =
                rsync_command(Path::new("/w/tilo"), &endpoint(), Some("/k/id x"), "wt-1").unwrap();
            let text = line.display();
            assert!(line.args.contains(&"--filter=:- .gitignore".to_owned()));
            assert!(line.args.contains(&"--exclude=/target/".to_owned()));
            assert!(line.args.contains(&"--delete".to_owned()));
            assert!(
                !line.args.iter().any(|a| a == "--delete-excluded"),
                "excluded files (the VM's target/) must survive"
            );
            assert!(
                text.ends_with("/w/tilo/ root@127.0.0.1:/var/lib/tilo-vm/src/wt-1/"),
                "{text}"
            );
            let rsh = &line.args[line.args.iter().position(|a| a == "--rsh").unwrap() + 1];
            assert!(rsh.contains("-p 2223"), "{rsh}");
            assert!(rsh.contains("-i '/k/id x'"), "{rsh}");
            assert!(!rsh.contains("root@"), "{rsh}");
            let path = &line.args[line.args.iter().position(|a| a == "--rsync-path").unwrap() + 1];
            assert!(
                path.contains("flock --nonblock /var/lib/tilo-vm/locks/src-wt-1.lock rsync"),
                "{path}"
            );
        }

        // Phase: windows_worktree_is_a_local_msys2_rsync_operand.
        #[cfg(windows)]
        {
            assert_eq!(
                rsync_source(Path::new(r"C:\Users\you\src\tilo")),
                "/c/Users/you/src/tilo"
            );
            let mut ssh = remote::ssh_command(
                &endpoint(),
                Some(r"C:\Users\you\.ssh\vm_ed25519"),
                &[],
                false,
            );
            use_msys_ssh(&mut ssh, Path::new(r"C:\msys64\usr\bin\ssh.exe"));
            assert_eq!(ssh.program, "/c/msys64/usr/bin/ssh.exe");
            assert!(ssh.args.contains(&"-i".into()));
            assert!(ssh.args.contains(&"/c/Users/you/.ssh/vm_ed25519".into()));
            assert!(
                ssh.args.contains(
                    &"UserKnownHostsFile=/c/Users/you/.ssh/vm_ed25519.known_hosts".into()
                )
            );
        }

        // Phase: job_commands_follow_the_configured_daemon.
        {
            assert_eq!(daemon_command(None, "build"), ["tilo-vmd", "build"]);
            assert_eq!(
                daemon_command(Some("sudo -n tilo-vmd rpc"), "ci-layer"),
                ["sudo", "-n", "tilo-vmd", "ci-layer"]
            );
        }
    }
}
