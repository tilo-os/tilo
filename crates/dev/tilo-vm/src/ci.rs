//! `swap --from sha:<commit>` and `--from ci:<tag>`: take a component's
//! files out of CI's dev image of that build,
//! `registry.tilo.so/tilo/tilo-vm:<tag>-ci`, the image `vm upgrade --from
//! sha:` moves a VM onto. It holds the CI OS image's binaries with the
//! dev-overrides rebuilds (the shell) on top, and dev images accept it
//! (os/dev-image's container policy rejects the bare `tilo-ci` OS image).
//!
//! CI tags main commits `main-<sha7>` and pull requests `pr-<n>-<sha7>`
//! (`build-image.yml`), each an index of the architectures that built, so a
//! pull takes the one the VM or device runs. The pull happens inside the VM
//! when its image has podman; otherwise on this host with podman, for the
//! VM's architecture, and the layer is uploaded.

use std::path::Path;

use crate::context::Worktree;
use crate::error::CliError;
use crate::hypervisor::{CommandLine, Runner};

pub const IMAGE: &str = "registry.tilo.so/tilo/tilo-vm";

/// What `--from` names, resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Worktree,
    Layer(String),
    /// A CI image tag, and the commit when the source named one.
    Ci {
        tag: String,
        commit: Option<String>,
    },
}

fn is_hex(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Parse `--from`. A `sha:` is resolved to a full commit through the
/// worktree's git when there is one.
///
/// # Errors
/// Unknown forms, and commits that are not hex or too short to tag.
pub fn parse(from: &str, worktree: Option<&Worktree>) -> Result<Source, CliError> {
    if from == "worktree" {
        return Ok(Source::Worktree);
    }
    if let Some(hash) = from.strip_prefix("layer:") {
        return Ok(Source::Layer(hash.to_owned()));
    }
    if let Some(tag) = from.strip_prefix("ci:") {
        let valid = !tag.is_empty()
            && tag.len() <= 128
            && tag
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b));
        return if valid {
            Ok(Source::Ci {
                tag: tag.to_owned(),
                commit: None,
            })
        } else {
            Err(CliError::InvalidSource(from.into()))
        };
    }
    if let Some(commit) = from.strip_prefix("sha:") {
        let full = worktree
            .and_then(|w| w.rev_parse(commit))
            .unwrap_or_else(|| commit.to_ascii_lowercase());
        if !is_hex(&full) || full.len() < 7 {
            return Err(CliError::InvalidSource(from.into()));
        }
        let short: String = full.chars().take(7).collect();
        return Ok(Source::Ci {
            tag: format!("main-{short}"),
            commit: (full.len() == 40).then_some(full),
        });
    }
    Err(CliError::InvalidSource(from.into()))
}

/// CI's dev image of build `tag` (`main-1a2b3c4`, or with its `-ci`).
pub fn image(tag: &str) -> String {
    format!("{IMAGE}:{}-ci", tag.strip_suffix("-ci").unwrap_or(tag))
}

/// podman's name for an architecture.
pub fn podman_arch(arch: &str) -> &str {
    match arch {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    }
}

/// Host-side extraction commands: pull, create a stopped container, copy
/// each path out (a trailing `/` copies the directory), remove it.
pub fn host_commands(
    image: &str,
    arch: &str,
    container: &str,
    paths: &[String],
    tree: &Path,
) -> (Vec<CommandLine>, CommandLine) {
    let mut commands = vec![
        CommandLine::new("podman", &["pull", "--arch", podman_arch(arch), image]),
        CommandLine::new(
            "podman",
            &["create", "--name", container, image, "/bin/true"],
        ),
    ];
    for path in paths {
        let relative = path.trim_start_matches('/').trim_end_matches('/');
        commands.push(
            CommandLine::new("podman", &["cp"])
                .arg(format!("{container}:/{relative}"))
                .arg(tree.join(relative).display().to_string()),
        );
    }
    let cleanup = CommandLine::new("podman", &["rm", "--force", container]);
    (commands, cleanup)
}

/// Run the host extraction; `tree` receives `usr/...`.
///
/// # Errors
/// podman failures; a missing path is [`CliError::MissingInImage`].
pub fn extract_on_host(
    runner: &mut dyn Runner,
    image: &str,
    arch: &str,
    paths: &[String],
    tree: &Path,
) -> Result<Option<String>, CliError> {
    let container = format!("tilo-vm-ci-{}", uuid::Uuid::new_v4().simple());
    let (commands, cleanup) = host_commands(image, arch, &container, paths, tree);
    let mut result = Ok(());
    for (index, command) in commands.iter().enumerate() {
        if let Some(parent) = command
            .args
            .last()
            .filter(|_| command.args.first().is_some_and(|a| a == "cp"))
            .and_then(|dest| Path::new(dest).parent())
        {
            std::fs::create_dir_all(parent)
                .map_err(CliError::io(format!("create {}", parent.display())))?;
        }
        // The pull streams its progress; the rest is quick.
        let ok = if index == 0 {
            runner.interactive(command)?
        } else {
            let output = runner.output(command)?;
            if !output.success && command.args.first().is_some_and(|a| a == "cp") {
                let path = paths
                    .get(index.saturating_sub(2))
                    .cloned()
                    .unwrap_or_default();
                result = Err(CliError::MissingInImage {
                    image: image.into(),
                    path,
                });
                break;
            }
            if !output.success {
                result = Err(CliError::CommandFailed {
                    command: command.display(),
                    status: output.status,
                    stderr: output.stderr.trim().to_owned(),
                });
                break;
            }
            true
        };
        if !ok {
            result = Err(CliError::CommandFailed {
                command: command.display(),
                status: "failed".into(),
                stderr: "the output above says why".into(),
            });
            break;
        }
    }
    let _ = runner.output(&cleanup);
    result?;
    let digest = runner
        .output(&CommandLine::new(
            "podman",
            &["image", "inspect", "--format", "{{.Digest}}", image],
        ))
        .ok()
        .filter(|o| o.success)
        .map(|o| o.stdout.trim().to_owned())
        .filter(|d| !d.is_empty());
    Ok(digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sources parse and host extraction copies each manifest path.
    #[test]
    fn ci_sources_parse_and_host_extraction_copies_each_manifest_path() {
        // Phase: sources_parse.
        {
            assert_eq!(parse("worktree", None).unwrap(), Source::Worktree);
            assert_eq!(
                parse("layer:ab12", None).unwrap(),
                Source::Layer("ab12".into())
            );
            assert_eq!(
                parse("ci:main-1a2b3c4", None).unwrap(),
                Source::Ci {
                    tag: "main-1a2b3c4".into(),
                    commit: None
                }
            );
            assert_eq!(
                parse("sha:1A2B3C4D5E", None).unwrap(),
                Source::Ci {
                    tag: "main-1a2b3c4".into(),
                    commit: None
                }
            );
            let full = "0123456789abcdef0123456789abcdef01234567";
            assert_eq!(
                parse(&format!("sha:{full}"), None).unwrap(),
                Source::Ci {
                    tag: "main-0123456".into(),
                    commit: Some(full.into())
                }
            );
            for bad in ["sha:xyz", "sha:12ab", "ci:", "ci:a b", "branch:main"] {
                assert!(
                    matches!(parse(bad, None), Err(CliError::InvalidSource(_))),
                    "{bad}"
                );
            }
        }

        // Phase: host_extraction_copies_each_manifest_path.
        #[cfg(unix)]
        {
            let (commands, cleanup) = host_commands(
                &image("main-1a2b3c4"),
                "aarch64",
                "c1",
                &["/usr/bin/app".into(), "/usr/lib/tilo-runtime/".into()],
                Path::new("/s/tree"),
            );
            let text: Vec<String> = commands.iter().map(CommandLine::display).collect();
            assert_eq!(
                text,
                [
                    "podman pull --arch arm64 registry.tilo.so/tilo/tilo-vm:main-1a2b3c4-ci",
                    "podman create --name c1 registry.tilo.so/tilo/tilo-vm:main-1a2b3c4-ci /bin/true",
                    "podman cp c1:/usr/bin/app /s/tree/usr/bin/app",
                    "podman cp c1:/usr/lib/tilo-runtime /s/tree/usr/lib/tilo-runtime",
                ]
            );
            assert_eq!(cleanup.display(), "podman rm --force c1");
        }
    }
}
