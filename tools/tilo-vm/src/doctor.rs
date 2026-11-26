//! `tilo-vm doctor`'s host-side checks, as pure functions of what was
//! probed, so every verdict and fix line is unit-tested. The checks that
//! need the VM stay in [`crate::app`].

use std::path::Path;

use serde::Serialize;

use crate::config::VmEntry;
use crate::disk::{self, Token};
use crate::error::{CliError, FORGE_TOKEN_USE, no_disk_help};
use crate::host::{self, Choice, HostFacts, HostKind, Missing};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub check: String,
    pub state: &'static str,
    pub detail: String,
    pub fix: Option<String>,
}

impl Check {
    pub fn ok(check: &str, detail: impl Into<String>) -> Self {
        Self {
            check: check.into(),
            state: "ok",
            detail: detail.into(),
            fix: None,
        }
    }

    pub fn warn(check: &str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            check: check.into(),
            state: "warn",
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }

    pub fn fail(check: &str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            check: check.into(),
            state: "fail",
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }
}

pub fn install_fix(facts: &HostFacts, tool: &str) -> String {
    let package = match (tool, facts.kind) {
        ("ssh", HostKind::Apt) => "openssh-client",
        ("ssh", HostKind::Pacman) => "openssh",
        ("ssh", _) => "openssh-clients",
        ("python3", HostKind::Pacman) => "python",
        (tool, _) => tool,
    };
    match facts.kind {
        HostKind::RpmOstree => {
            format!("this host is image-based: rpm-ostree install {package}, then reboot")
        }
        HostKind::Bootc => {
            format!("this host is image-based (bootc): add {package} to your host image")
        }
        HostKind::Apt => format!("sudo apt install {package}"),
        HostKind::Pacman => format!("sudo pacman -S {package}"),
        HostKind::Zypper => format!("sudo zypper install {package}"),
        HostKind::Macos => format!("brew install {package}"),
        HostKind::Windows => format!("install {package} (winget) and put it on PATH"),
        HostKind::Dnf => format!("sudo dnf install {package}"),
        HostKind::Unknown => format!("install {package} with your package manager"),
    }
}

pub fn host_checks(facts: &HostFacts) -> Vec<Check> {
    let mut checks = vec![Check::ok(
        "host",
        format!("{} on {}", facts.kind.describe(), facts.arch),
    )];
    for (tool, purpose) in [
        ("ssh", "open a shell with tilo-vm vm ssh"),
        ("git", "name your account and record provenance"),
        ("curl", "download the published dev disk"),
    ] {
        checks.push(match facts.first(tool) {
            Some(path) => Check::ok(tool, format!("{} ({purpose})", path.display())),
            None => Check::fail(
                tool,
                format!("not on PATH (needed to {purpose})"),
                install_fix(facts, tool),
            ),
        });
    }
    if facts.os == "linux" {
        let cargo = crate::build::cargo_program();
        if cargo != "cargo" {
            checks.push(Check::ok(
                "host cargo",
                format!(
                    "swap --builder host runs {cargo} (TILO_VM_CARGO; unset builds on this machine with cargo)"
                ),
            ));
        }
        for tool in ["cargo", "python3"] {
            checks.push(match facts.first(tool) {
                Some(path) => Check::ok(
                    tool,
                    format!("{} (for swap --builder host)", path.display()),
                ),
                None => Check::warn(
                    tool,
                    "not on PATH: swap --builder host cannot build here (the in-VM builder still can)",
                    install_fix(facts, tool),
                ),
            });
        }
    } else {
        checks.push(Check::ok(
            "host builds",
            "not possible on this OS; swaps build in the VM (--builder vm, the default)",
        ));
    }
    checks.push(match facts.first("podman") {
        Some(path) => Check::ok(
            "podman",
            format!(
                "{} (pulls CI layers on this host when the VM has no podman)",
                path.display()
            ),
        ),
        None => Check::warn(
            "podman",
            "not on PATH: swap --from ci:/sha: works only when the VM's image has podman",
            install_fix(facts, "podman"),
        ),
    });
    checks.extend(hypervisor_checks(facts));
    checks
}

fn verdict(name: &str, result: Result<String, Missing>) -> Check {
    match result {
        Ok(detail) => Check::ok(name, detail),
        Err(m) => Check::fail(name, m.detail, m.fix),
    }
}

pub fn hypervisor_checks(facts: &HostFacts) -> Vec<Check> {
    if facts.os != "linux" {
        return vec![verdict(
            "hypervisor",
            host::auto(facts).map(|c| format!("vm create uses {}", c.describe())),
        )];
    }
    let tools = host::libvirt_tools(facts);
    let have_virsh = facts.first("virsh").is_some();
    let mut checks = vec![verdict(
        "hypervisor",
        tools.map(|tools| {
            let choice = Choice {
                adapter: "libvirt",
                tools,
            };
            format!(
                "vm create uses {}; {}",
                choice.describe(),
                host::VIRT_MANAGER_TIP
            )
        }),
    )];
    checks.push(verdict(
        "kvm",
        host::kvm_usable(facts).map(|()| "/dev/kvm is usable".to_owned()),
    ));
    if have_virsh {
        checks.push(verdict(
            "libvirt session",
            host::session_domain(facts, &facts.arch, false),
        ));
    }
    checks
}

pub fn vm_adapter_checks(
    entry: &VmEntry,
    verdict: Result<Choice, Missing>,
    has_vm_sh: bool,
) -> Vec<Check> {
    let mut checks = vec![match verdict {
        Ok(choice) => Check::ok("vm hypervisor", choice.describe()),
        Err(missing) => Check::fail(
            "vm hypervisor",
            format!("{}: {}", missing.adapter, missing.detail),
            missing.fix,
        ),
    }];
    if entry.hypervisor == "qemu" && !has_vm_sh {
        checks.push(Check::fail(
            "vm.sh",
            "no os/dev/vm.sh here; the qemu adapter drives it",
            "run tilo-vm from a Tilo worktree",
        ));
    }
    checks
}

pub fn credential_checks(token: Option<&Token>) -> Vec<Check> {
    let forge = match token {
        Some(token) => Check::ok(
            "forge token",
            format!("from {}; {FORGE_TOKEN_USE}", token.source),
        ),
        None => Check::warn(
            "forge token",
            format!(
                "none found (FORGEJO_TOKEN, fgj or tea login); a forge token is {FORGE_TOKEN_USE}"
            ),
            "export FORGEJO_TOKEN=<forge token with package read access>, or log in with fgj or tea",
        ),
    };
    vec![forge]
}

pub fn disk_check(listing: Result<(u16, String), CliError>, token: Option<&Token>) -> Check {
    let name = "published disk";
    match listing {
        Ok((200, body)) => match disk::newest_tag(&body) {
            Some(tag) => Check::ok(
                name,
                format!("newest dev disk: {tag} (vm create without --tag takes it)"),
            ),
            None if token.is_none() => Check::warn(
                name,
                "none visible: without a forge token the forge hides private packages",
                "export FORGEJO_TOKEN=<forge token with package read access>, or log in with fgj or tea",
            ),
            None => Check::warn(
                name,
                "no dev disk is published yet: vm create cannot download one",
                no_disk_help(),
            ),
        },
        Ok((status @ (401 | 403), _)) => {
            let error = CliError::ForgeAuth {
                url: disk::VERSIONS.into(),
                status,
                token: token.map(|t| t.source.clone()),
            };
            Check::warn(name, error.to_string(), error.hint())
        }
        Ok((status, _)) => Check::warn(
            name,
            format!("the forge answered HTTP {status} to {}", disk::VERSIONS),
            "rerun tilo-vm doctor; if it repeats, the forge is having trouble",
        ),
        Err(error) => Check::warn(
            name,
            format!("could not ask the forge: {error}"),
            error.hint(),
        ),
    }
}

#[cfg(unix)]
pub fn root_owned_build_dirs(
    roots: &[std::path::PathBuf],
    depth: usize,
) -> Vec<std::path::PathBuf> {
    roots
        .iter()
        .filter(|root| holds_files_of(root, 0, depth))
        .cloned()
        .collect()
}

#[cfg(not(unix))]
pub fn root_owned_build_dirs(
    _roots: &[std::path::PathBuf],
    _depth: usize,
) -> Vec<std::path::PathBuf> {
    Vec::new()
}

#[cfg(unix)]
fn holds_files_of(root: &Path, uid: u32, depth: usize) -> bool {
    use std::os::unix::fs::MetadataExt;
    let mut pending = vec![(root.to_owned(), 0_usize)];
    while let Some((path, level)) = pending.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.uid() == uid {
            return true;
        }
        if meta.is_dir() && level < depth {
            let Ok(entries) = std::fs::read_dir(&path) else {
                continue;
            };
            let next = level.saturating_add(1);
            pending.extend(entries.flatten().map(|entry| (entry.path(), next)));
        }
    }
    false
}

pub fn ownership_check(found: &[std::path::PathBuf], user: &str) -> Check {
    const NAME: &str = "build file ownership";
    if found.is_empty() {
        return Check::ok(
            NAME,
            "worktree targets and the compiler cache belong to you",
        );
    }
    let paths: Vec<String> = found.iter().map(|p| p.display().to_string()).collect();
    let shown = paths.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
    let more = paths.len().saturating_sub(3);
    let detail = if more == 0 {
        format!("root owns files in {shown}; cargo as {user} cannot overwrite them")
    } else {
        format!("root owns files in {shown} and {more} more; cargo as {user} cannot overwrite them")
    };
    Check::fail(
        NAME,
        detail,
        format!("sudo chown -R {user}: {}", paths.join(" ")),
    )
}

/// The extension of the file that proves a build directory is cargo's: the
/// dep-info a profile's `deps/` carries.
const CARGO_OUTPUT: &str = "d";

pub fn contended_build_dirs(mine: &Path, others: &[std::path::PathBuf]) -> Vec<std::path::PathBuf> {
    let here = identity(mine);
    let mut found: Vec<std::path::PathBuf> = Vec::new();
    for other in others {
        if identity(other) == here || holds_cargo_outputs(other).is_empty() {
            continue;
        }
        found.push(other.clone());
    }
    found.sort();
    found.dedup();
    found
}

fn identity(dir: &Path) -> std::path::PathBuf {
    std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_owned())
}

fn holds_cargo_outputs(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for level in ["debug/deps", "release/deps", "deps"] {
        let deps = dir.join(level);
        let Ok(entries) = std::fs::read_dir(&deps) else {
            continue;
        };
        found.extend(entries.flatten().map(|entry| entry.path()).filter(|path| {
            path.extension()
                .is_some_and(|kind| kind == std::ffi::OsStr::new(CARGO_OUTPUT))
                && path.is_file()
        }));
    }
    found.truncate(3);
    found
}

pub fn contended_build_check(found: &[std::path::PathBuf]) -> Check {
    const NAME: &str = "build target contention";
    if found.is_empty() {
        return Check::ok(NAME, "no other worktree shares this one's cargo outputs");
    }
    let paths: Vec<String> = found.iter().map(|p| p.display().to_string()).collect();
    let shown = paths.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
    let more = paths.len().saturating_sub(3);
    let detail = if more == 0 {
        format!("another worktree writes the same cargo outputs in {shown}")
    } else {
        format!("another worktree writes the same cargo outputs in {shown} and {more} more")
    };
    Check::fail(
        NAME,
        detail,
        "give each worktree its own target: remove the linked one and let cargo rebuild it",
    )
}
