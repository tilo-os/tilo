//! `swap --from sha:<commit>` and `--from ci:<tag>`: take a component's
//! files out of CI's dev image of that build,
//! `registry.tilo.so/tilo/tilo-vm:<tag>-ci`, the image `vm upgrade --from
//! sha:` moves a VM onto. It holds the CI OS image's binaries with the
//! dev-overrides rebuilds (the shell) on top, and dev images accept it
//! (os/dev-image's container policy rejects the bare `tilo-ci` OS image).
//!
//! Main commits are tagged `main-<sha7>` (t's publish-os-dev-<arch>), each
//! an index of the architectures that built, so a
//! pull takes the one the VM or device runs. The pull happens inside the VM
//! when its image has podman; otherwise on this host with podman, for the
//! VM's architecture, and the layer is uploaded.

use std::path::Path;

use crate::context::Worktree;
use crate::error::CliError;
use crate::hypervisor::{CommandLine, Runner};

pub const IMAGE: &str = "registry.tilo.so/tilo/tilo-vm";
/// Branches' own builds (t's publish-os-ref-<target>): a repository of their own, never one a
/// device follows a channel from.
pub const REF_IMAGE: &str = "registry.tilo.so/tilo/tilo-vm-ref";

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

/// A branch as the part of a tag naming it: lower case, every run of other characters a dash,
/// at most 48 characters; the same as tools/ci/publish/lib.sh `ref_slug`, which names the
/// branch's own builds `ref-<slug>-<sha7>-<target>`.
pub fn ref_slug(branch: &str) -> String {
    let mut slug = String::new();
    for c in branch.chars().map(|c| c.to_ascii_lowercase()) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_start_matches(['-', '.']).trim_end_matches('-');
    slug.chars()
        .take(48)
        .collect::<String>()
        .trim_end_matches('-')
        .to_owned()
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
/// each path out (a trailing `/` copies the directory); then the cleanup
/// that removes the container and the image, which only the layer outlives.
pub fn host_commands(
    image: &str,
    arch: &str,
    container: &str,
    paths: &[String],
    tree: &Path,
) -> (Vec<CommandLine>, Vec<CommandLine>) {
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
    let cleanup = vec![
        CommandLine::new("podman", &["rm", "--force", container]),
        CommandLine::new("podman", &["rmi", image]),
    ];
    (commands, cleanup)
}

/// What the pulled image says about itself, for the layer's provenance.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Pulled {
    pub digest: Option<String>,
    /// `org.opencontainers.image.revision`: the commit CI built.
    pub revision: Option<String>,
}

/// The registry's manifest of `image`, without pulling it.
pub fn manifest_command(image: &str) -> CommandLine {
    CommandLine::new("skopeo", &["inspect", "--raw"]).arg(format!("docker://{image}"))
}

/// The digest of `arch`'s image in a [`manifest_command`] answer: the
/// matching entry of an index, or the digest of a single-image manifest.
/// It is the digest a pull for `arch` reports, so it names the content.
pub fn arch_digest(raw: &str, arch: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let Some(manifests) = value.get("manifests").and_then(serde_json::Value::as_array) else {
        use sha2::Digest as _;
        return Some(format!(
            "sha256:{}",
            hex::encode(sha2::Sha256::digest(raw.trim_end_matches('\n')))
        ));
    };
    manifests
        .iter()
        .find(|m| {
            let platform = m.get("platform");
            let field = |key: &str| {
                platform
                    .and_then(|p| p.get(key))
                    .and_then(serde_json::Value::as_str)
            };
            field("architecture") == Some(podman_arch(arch))
                && field("os").is_none_or(|os| os == "linux")
        })
        .and_then(|m| m.get("digest")?.as_str())
        .map(str::to_owned)
}

/// The digest `image` has for `arch` in the registry now, or `None` when
/// the registry (or skopeo) cannot say.
pub fn registry_digest(runner: &mut dyn Runner, image: &str, arch: &str) -> Option<String> {
    let output = runner.output(&manifest_command(image)).ok()?;
    output
        .success
        .then(|| arch_digest(&output.stdout, arch))
        .flatten()
}

/// `podman image inspect` of the pulled image: its digest and revision.
pub fn inspect_command(image: &str) -> CommandLine {
    CommandLine::new(
        "podman",
        &[
            "image",
            "inspect",
            "--format",
            "{{.Digest}} {{index .Labels \"org.opencontainers.image.revision\"}}",
            image,
        ],
    )
}

/// The [`inspect_command`] answer.
pub fn parse_inspect(output: &str) -> Pulled {
    let mut words = output.split_whitespace();
    let mut next = || {
        words
            .next()
            .filter(|w| !w.is_empty() && *w != "<no" && *w != "<no value>")
            .map(str::to_owned)
    };
    Pulled {
        digest: next().filter(|d| d.starts_with("sha256:")),
        revision: next().filter(|r| r.bytes().all(|b| b.is_ascii_hexdigit())),
    }
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
) -> Result<Pulled, CliError> {
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
    // Read before the cleanup removes the image.
    let pulled = runner
        .output(&inspect_command(image))
        .ok()
        .filter(|o| o.success)
        .map(|o| parse_inspect(&o.stdout))
        .unwrap_or_default();
    for command in &cleanup {
        let _ = runner.output(command);
    }
    result?;
    Ok(pulled)
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_branch_names_its_builds_as_the_publishing_check_does() {
        for (branch, slug) in [
            ("feature/Foo_bar", "feature-foo-bar"),
            ("ada/dev-delivery", "ada-dev-delivery"),
            ("--x--", "x"),
            ("a/b/c.d", "a-b-c.d"),
        ] {
            assert_eq!(ref_slug(branch), slug, "{branch}");
        }
        assert_eq!(ref_slug(&"a".repeat(60)).len(), 48);
    }

    use super::*;

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
            let cleanup: Vec<String> = cleanup.iter().map(CommandLine::display).collect();
            assert_eq!(
                cleanup,
                [
                    "podman rm --force c1",
                    "podman rmi registry.tilo.so/tilo/tilo-vm:main-1a2b3c4-ci"
                ]
            );
            assert_eq!(
                parse_inspect("sha256:ab12 fff3babf03ebbefb1e3757b8a9e0263c2c815943\n"),
                Pulled {
                    digest: Some("sha256:ab12".into()),
                    revision: Some("fff3babf03ebbefb1e3757b8a9e0263c2c815943".into()),
                }
            );
            let index = r#"{"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[
                {"digest":"sha256:5988","platform":{"architecture":"arm64","os":"linux"}},
                {"digest":"sha256:8b92","platform":{"architecture":"amd64","os":"linux"}}]}"#;
            assert_eq!(
                arch_digest(index, "aarch64").as_deref(),
                Some("sha256:5988")
            );
            assert_eq!(arch_digest(index, "x86_64").as_deref(), Some("sha256:8b92"));
            assert_eq!(arch_digest(index, "riscv64"), None);
            assert_eq!(
                arch_digest("{}", "aarch64").as_deref(),
                Some("sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a")
            );
            assert_eq!(
                manifest_command("registry.tilo.so/tilo/tilo-vm:main-1a2b3c4-ci").display(),
                "skopeo inspect --raw docker://registry.tilo.so/tilo/tilo-vm:main-1a2b3c4-ci"
            );
            assert_eq!(
                parse_inspect("sha256:ab12 <no value>\n"),
                Pulled {
                    digest: Some("sha256:ab12".into()),
                    revision: None,
                }
            );
        }
    }
}
