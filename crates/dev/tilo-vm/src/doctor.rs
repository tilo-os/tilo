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

/// How to install one host package on this host.
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
        HostKind::Windows if tool == "rsync" => {
            "install MSYS2 rsync and openssh in %USERPROFILE%\\msys64\\usr\\bin (pacman -S rsync openssh)".into()
        }
        HostKind::Windows => format!("install {package} (winget) and put it on PATH"),
        HostKind::Dnf => format!("sudo dnf install {package}"),
        HostKind::Unknown => format!("install {package} with your package manager"),
    }
}

/// Host tools, each with the path it resolves to, and the hypervisor.
pub fn host_checks(facts: &HostFacts) -> Vec<Check> {
    let mut checks = vec![Check::ok(
        "host",
        format!("{} on {}", facts.kind.describe(), facts.arch),
    )];
    for (tool, purpose) in [
        ("ssh", "reach the VM"),
        ("git", "name your slot and record provenance"),
        ("rsync", "sync this worktree into the VM for in-VM builds"),
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
    if facts.os == "windows" {
        checks.push(match crate::host::rsync_ssh_program() {
            Some(path) => Check::ok(
                "rsync SSH",
                format!("{} (MSYS2 transport for rsync)", path.display()),
            ),
            None => Check::fail(
                "rsync SSH",
                "MSYS2 ssh.exe is missing beside rsync.exe; native Windows OpenSSH closes the rsync pipe",
                "install MSYS2 openssh with pacman -S openssh",
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

/// The adapter `vm create` uses by default, with every tool's resolved
/// path. On Linux that is libvirt's rootless session, so three checks: its
/// tools, KVM access, and that the session's QEMU driver answers
/// `virsh capabilities` with a KVM guest for this host's architecture. The
/// storage and network drivers are never needed, so never probed.
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

/// The registered VM's own adapter, plus what the qemu adapter needs from
/// the worktree.
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

/// The forge token, with the operations that read it. Image pulls need no
/// credential: registry.tilo.so is public.
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

/// Is a dev disk published? `listing` is the forge's answer to
/// [`disk::VERSIONS`]; `token` whether one was sent.
pub fn disk_check(
    listing: Result<(u16, String), CliError>,
    token: Option<&Token>,
    local_build: Option<&str>,
) -> Check {
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
                no_disk_help(local_build),
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

/// Where the local dev-disk build script is, when this checkout has one.
pub fn local_build(root: &Path) -> Option<String> {
    let script = root.join(crate::error::LOCAL_BUILD_SCRIPT);
    script.is_file().then(|| script.display().to_string())
}

/// Build directories that hold files owned by root: every worktree's
/// `target/` and the shared compiler cache. Older in-VM builders ran cargo
/// as root, and what they left makes the next `cargo` as yourself fail
/// with "permission denied". Only the first `depth` levels are looked at,
/// which is where cargo writes (`target/<profile>/deps/*.d`).
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

/// Whether `root` or anything within `depth` levels below it belongs to `uid`.
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
            pending.extend(entries.flatten().map(|entry| (entry.path(), level + 1)));
        }
    }
    false
}

/// The verdict on [`root_owned_build_dirs`]' findings, with the command
/// that gives them back to `user`.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::tests::{LIBVIRT, linux};

    fn find<'a>(checks: &'a [Check], name: &str) -> &'a Check {
        checks
            .iter()
            .find(|c| c.check == name)
            .unwrap_or_else(|| panic!("no {name} check in {checks:?}"))
    }

    /// Tools print their resolved path or an OS-specific fix; hypervisor checks cover tools, KVM and the
    /// session; verdicts ignore virt-install; a registered QEMU VM needs vm.sh.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn doctor_checks_tools_hypervisors_and_vm_registration() {
        // Phase: tools_print_their_resolved_path_or_an_os_specific_fix.
        {
            let facts = linux(
                HostKind::RpmOstree,
                &[("ssh", "/usr/bin/ssh"), ("git", "/usr/bin/git")],
            );
            let checks = host_checks(&facts);
            assert_eq!(find(&checks, "ssh").detail, "/usr/bin/ssh (reach the VM)");
            let rsync = find(&checks, "rsync");
            assert_eq!(rsync.state, "fail");
            assert!(
                rsync
                    .fix
                    .as_deref()
                    .unwrap()
                    .starts_with("this host is image-based: rpm-ostree install rsync"),
                "{rsync:?}"
            );
            let apt = linux(HostKind::Apt, &[]);
            assert_eq!(
                find(&host_checks(&apt), "ssh").fix.as_deref(),
                Some("sudo apt install openssh-client")
            );
        }

        // Phase: hypervisor_checks_cover_tools_kvm_and_the_session.
        {
            let facts = linux(HostKind::Dnf, LIBVIRT);
            let checks = hypervisor_checks(&facts);
            let hv = find(&checks, "hypervisor");
            assert_eq!(hv.state, "ok");
            assert!(
                hv.detail.contains("libvirt (qemu:///session)"),
                "{}",
                hv.detail
            );
            assert!(
                hv.detail.contains("qemu-img = /usr/bin/qemu-img"),
                "{}",
                hv.detail
            );
            assert!(hv.detail.contains("virt-manager"), "{}", hv.detail);
            assert_eq!(find(&checks, "kvm").state, "ok");
            let session = find(&checks, "libvirt session");
            assert_eq!(session.state, "ok");
            assert!(
                session.detail.contains("x86_64 guests with kvm"),
                "{session:?}"
            );

            let mut broken = linux(HostKind::Dnf, LIBVIRT);
            broken.kvm = Some(false);
            let stderr = "error: failed to connect to the hypervisor\nerror: Failed to connect socket to '/run/user/1000/libvirt/virtqemud-sock': Connection refused";
            broken.session = Some(Err(stderr.into()));
            let checks = hypervisor_checks(&broken);
            assert_eq!(find(&checks, "kvm").state, "fail");
            let session = find(&checks, "libvirt session");
            assert_eq!(session.state, "fail");
            assert!(session.detail.contains(stderr), "{session:?}");
            assert!(
                session.fix.as_deref().unwrap().contains(stderr),
                "{session:?}"
            );

            let mut no_kvm = linux(HostKind::Dnf, LIBVIRT);
            no_kvm.session = Some(Ok(vec![host::Guest {
                arch: "x86_64".into(),
                domains: vec!["qemu".into()],
            }]));
            let session = find(&hypervisor_checks(&no_kvm), "libvirt session").clone();
            assert_eq!(session.state, "fail");
            assert!(session.detail.contains("not kvm"), "{session:?}");

            // An image-based host with only an Android SDK's qemu-img on PATH.
            let stray = linux(
                HostKind::RpmOstree,
                &[("qemu-img", "/home/u/Android/Sdk/emulator/qemu-img")],
            );
            let checks = hypervisor_checks(&stray);
            let hv = find(&checks, "hypervisor");
            assert_eq!(hv.state, "fail");
            assert!(
                hv.fix
                    .as_deref()
                    .unwrap()
                    .contains("rpm-ostree install libvirt-daemon-kvm libvirt-client virt-manager"),
                "{hv:?}"
            );
            assert!(checks.iter().all(|c| c.check != "libvirt session"));
        }

        // Phase: doctor_verdicts_ignore_virt_install.
        {
            let mut with_it = LIBVIRT.to_vec();
            with_it.push(("virt-install", "/usr/bin/virt-install"));
            let with = hypervisor_checks(&linux(HostKind::Dnf, &with_it));
            let without = hypervisor_checks(&linux(HostKind::Dnf, LIBVIRT));
            assert_eq!(with, without);
            assert!(without.iter().all(|c| c.state == "ok"), "{without:?}");
            let text = format!("{without:?}");
            assert!(!text.contains("virt-install"), "{text}");

            // virt-install alone is not libvirt: the fix names what is needed.
            let only = hypervisor_checks(&linux(
                HostKind::Apt,
                &[("virt-install", "/usr/bin/virt-install")],
            ));
            let hv = find(&only, "hypervisor");
            assert_eq!(hv.state, "fail");
            let fix = hv.fix.as_deref().unwrap();
            assert!(
                !fix.contains("virtinst") && !fix.contains("virt-install"),
                "{fix}"
            );
            assert!(fix.contains("libvirt-clients"), "{fix}");
        }

        // Phase: a_registered_qemu_vm_needs_vm_sh.
        {
            let entry = VmEntry {
                hypervisor: "qemu".into(),
                ..VmEntry::default()
            };
            let facts = linux(HostKind::Dnf, &[]);
            let verdict = host::evaluate(&facts, "qemu");
            let checks = vm_adapter_checks(&entry, verdict, false);
            assert_eq!(checks[0].state, "fail");
            assert_eq!(find(&checks, "vm.sh").state, "fail");
        }
    }

    /// Credentials say which operations read them; the published-disk check covers each answer; a local
    /// build is offered only when the script exists.
    #[test]
    fn doctor_explains_credentials_disks_and_local_builds() {
        // Phase: credentials_say_which_operations_read_them.
        {
            let token = Token {
                value: "t".into(),
                source: "FORGEJO_TOKEN".into(),
            };
            let checks = credential_checks(Some(&token));
            assert_eq!(checks.len(), 1);
            assert!(
                checks[0].detail.contains("dev disk downloads"),
                "{:?}",
                checks[0]
            );
        }

        // Phase: published_disk_check_covers_each_answer.
        {
            let token = Token {
                value: "t".into(),
                source: "fgj".into(),
            };
            let listing =
                r#"[{"name":"tilo-vm-disk","version":"main-1a2b3c4","created_at":"2026-09-22"}]"#;
            let ok = disk_check(Ok((200, listing.into())), Some(&token), None);
            assert_eq!(ok.state, "ok");
            assert!(ok.detail.contains("main-1a2b3c4"));
            let none = disk_check(Ok((200, "[]".into())), Some(&token), None);
            assert_eq!(none.state, "warn");
            let fix = none.fix.unwrap();
            assert!(fix.contains("Release / Dev VM disk"), "{fix}");
            assert!(
                fix.contains("no local dev-image build is available"),
                "{fix}"
            );
            let hidden = disk_check(Ok((200, "[]".into())), None, None);
            assert!(hidden.fix.unwrap().contains("FORGEJO_TOKEN"));
            let refused = disk_check(Ok((401, String::new())), Some(&token), None);
            assert!(refused.fix.unwrap().contains("token from fgj"));
            let offline = disk_check(
                Err(CliError::ToolMissing {
                    tool: "curl".into(),
                    purpose: "p".into(),
                }),
                None,
                None,
            );
            assert_eq!(offline.state, "warn");
        }

        // Phase: local_build_is_offered_only_when_the_script_exists.
        {
            let dir = tempfile::tempdir().unwrap();
            assert_eq!(local_build(dir.path()), None);
            std::fs::create_dir_all(dir.path().join("os/dev")).unwrap();
            std::fs::write(dir.path().join("os/dev/build-dev-image.sh"), "").unwrap();
            assert!(
                local_build(dir.path())
                    .unwrap()
                    .ends_with("os/dev/build-dev-image.sh")
            );
        }
    }
}

#[cfg(test)]
mod ownership_tests {
    use super::*;

    /// Root-owned files in a target or the compiler cache fail the check
    /// with the exact chown that fixes them; none passes.
    #[test]
    fn root_owned_build_files_fail_with_the_chown_that_fixes_them() {
        assert_eq!(ownership_check(&[], "xaver").state, "ok");
        let found = vec![
            std::path::PathBuf::from("/w/a/target"),
            std::path::PathBuf::from("/var/cache/tilo-vm/sccache"),
        ];
        let check = ownership_check(&found, "xaver");
        assert_eq!(check.state, "fail");
        assert!(check.detail.contains("/w/a/target"));
        assert_eq!(
            check.fix.as_deref(),
            Some("sudo chown -R xaver: /w/a/target /var/cache/tilo-vm/sccache")
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_walk_finds_an_owner_only_within_its_depth() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let deep = dir.path().join("debug/deps");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("x.d"), "").unwrap();
        let me = std::fs::metadata(dir.path()).unwrap().uid();
        assert!(holds_files_of(dir.path(), me, 0));
        if me != 0 {
            assert!(!holds_files_of(dir.path(), 0, 3));
            assert!(root_owned_build_dirs(&[dir.path().to_owned()], 3).is_empty());
        }
        assert!(!holds_files_of(&dir.path().join("missing"), me, 3));
    }
}
