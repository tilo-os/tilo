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

/// `rsync` that mirrors the worktree into `SRC_DIR/<id>/` in the VM:
/// `.gitignore` rules apply, `.git` and `target/` stay behind (the VM's
/// `target/` is its own build dir and is never deleted by the sync), and
/// the remote side holds the worktree's build lock while it writes.
pub fn rsync_command(
    root: &Path,
    endpoint: &SshEndpoint,
    key: Option<&str>,
    id: &str,
) -> CommandLine {
    let ssh = remote::ssh_command(endpoint, key, &[], false);
    let mut words = vec![sh_quote(&ssh.program)];
    // ssh_args ends with user@host, which rsync puts in the destination.
    let options = ssh.args.len().saturating_sub(1);
    words.extend(ssh.args.iter().take(options).map(|a| sh_quote(a)));
    let dest_dir = format!("{SRC_DIR}/{id}");
    let locks = "/var/lib/tilo-vm/locks";
    let rsync_path =
        format!("mkdir -p {locks} {dest_dir} && flock --nonblock {locks}/src-{id}.lock rsync");
    let mut source = rsync_source(root);
    if !source.ends_with('/') {
        source.push('/');
    }
    CommandLine::new(
        "rsync",
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
    .arg(words.join(" "))
    .arg("--rsync-path")
    .arg(rsync_path)
    .arg(source)
    .arg(format!("{}@{}:{dest_dir}/", endpoint.user, endpoint.host))
}

/// MSYS2 rsync parses `C:\...` as a remote `host:path` operand. Use its
/// `/c/...` mount syntax for a Windows worktree passed as a local source.
fn rsync_source(root: &Path) -> String {
    let path = root.display().to_string();
    #[cfg(windows)]
    {
        let bytes = path.as_bytes();
        if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            let drive = (bytes[0] as char).to_ascii_lowercase();
            return format!("/{drive}{}", path[2..].replace('\\', "/"));
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

    #[test]
    fn worktree_ids_are_readable_and_unique_per_host() {
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

    #[test]
    fn rsync_respects_gitignore_and_leaves_target_alone() {
        let line = rsync_command(Path::new("/w/tilo"), &endpoint(), Some("/k/id x"), "wt-1");
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

    #[cfg(windows)]
    #[test]
    fn windows_worktree_is_a_local_msys2_rsync_operand() {
        let line = rsync_command(
            Path::new(r"C:\Users\xaver\qos-tilo-main"),
            &endpoint(),
            None,
            "wt-1",
        );
        assert_eq!(
            line.args[line.args.len() - 2],
            "/c/Users/xaver/qos-tilo-main/"
        );
    }

    #[test]
    fn job_commands_follow_the_configured_daemon() {
        assert_eq!(daemon_command(None, "build"), ["tilo-vmd", "build"]);
        assert_eq!(
            daemon_command(Some("sudo -n tilo-vmd rpc"), "ci-layer"),
            ["sudo", "-n", "tilo-vmd", "ci-layer"]
        );
    }
}
