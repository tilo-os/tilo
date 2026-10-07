//! The host's virtio-fs shares, mounted at the host's own paths.

use std::path::Path;
use std::process::Command;

use legacy_agent_protocol::GuestShare;

/// What to do to get from `current` to `desired`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MountPlan {
    pub unmount: Vec<GuestShare>,
    pub mount: Vec<GuestShare>,
}

/// Unmount everything no longer wanted (deepest path first), then mount
/// what is new (shallowest first, so a share inside another lands on top).
/// A share whose tag, path or mode changed is unmounted and mounted again.
#[must_use]
pub fn plan(current: &[GuestShare], desired: &[GuestShare]) -> MountPlan {
    let mut unmount: Vec<GuestShare> = current
        .iter()
        .filter(|share| !desired.contains(share))
        .cloned()
        .collect();
    unmount.sort_by_key(|share| std::cmp::Reverse(depth(&share.path)));
    let mut mount: Vec<GuestShare> = desired
        .iter()
        .filter(|share| !current.contains(share))
        .cloned()
        .collect();
    mount.sort_by_key(|share| depth(&share.path));
    MountPlan { unmount, mount }
}

fn depth(path: &str) -> usize {
    Path::new(path).components().count()
}

/// `mount -t virtiofs [-o ro] <tag> <path>`.
#[must_use]
pub fn mount_argv(share: &GuestShare) -> Vec<String> {
    let mut argv = vec!["mount".to_string(), "-t".into(), "virtiofs".into()];
    if share.read_only {
        argv.extend(["-o".to_string(), "ro".to_string()]);
    }
    argv.extend([share.tag.clone(), share.path.clone()]);
    argv
}

/// Apply `plan`, returning the shares mounted afterwards and the failures.
pub fn apply(current: &[GuestShare], plan: &MountPlan) -> (Vec<GuestShare>, Vec<String>) {
    let mut mounted: Vec<GuestShare> = current.to_vec();
    let mut failures = Vec::new();
    for share in &plan.unmount {
        // Lazy: an app with an open file must not keep a revoked share alive
        // for new opens.
        match run(&["umount", "-l", &share.path]) {
            Ok(()) => mounted.retain(|other| other != share),
            Err(error) => failures.push(format!("unmount {}: {error}", share.path)),
        }
    }
    for share in &plan.mount {
        if let Err(error) = std::fs::create_dir_all(&share.path) {
            failures.push(format!("create {}: {error}", share.path));
            continue;
        }
        let argv = mount_argv(share);
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        match mount_when_present(&argv) {
            Ok(()) => mounted.push(share.clone()),
            Err(error) => failures.push(format!("mount {}: {error}", share.path)),
        }
    }
    (mounted, failures)
}

/// The host attaches a share live, just before asking for the mount; the
/// guest's PCI hotplug may not have bound the device yet (notably right
/// after boot), so an unknown tag is retried for a few seconds.
fn mount_when_present(argv: &[&str]) -> Result<(), String> {
    let mut last = String::new();
    for _ in 0..30 {
        match run(argv) {
            Ok(()) => return Ok(()),
            Err(error) => last = error,
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    Err(last)
}

fn run(argv: &[&str]) -> Result<(), String> {
    let (name, rest) = argv.split_first().ok_or("empty command")?;
    let output = Command::new(name)
        .args(rest)
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn share(tag: &str, path: &str, read_only: bool) -> GuestShare {
        GuestShare {
            tag: tag.into(),
            path: path.into(),
            read_only,
        }
    }

    #[test]
    fn shares_converge_on_the_desired_set() {
        // phase: new shares mount parent-first, revoked ones unmount child-first
        {
            let current = vec![share("a", "/h/a", false), share("b", "/h/a/b", false)];
            let desired = vec![share("c", "/h/c/d", false), share("e", "/h/c", true)];
            let plan = plan(&current, &desired);
            assert_eq!(
                plan.unmount,
                [share("b", "/h/a/b", false), share("a", "/h/a", false)]
            );
            assert_eq!(
                plan.mount,
                [share("e", "/h/c", true), share("c", "/h/c/d", false)]
            );
        }
        // phase: an unchanged share is left alone, a changed mode remounts
        {
            let current = vec![share("a", "/h/a", false), share("b", "/h/b", false)];
            let desired = vec![share("a", "/h/a", false), share("b", "/h/b", true)];
            let plan = plan(&current, &desired);
            assert_eq!(plan.unmount, [share("b", "/h/b", false)]);
            assert_eq!(plan.mount, [share("b", "/h/b", true)]);
        }
        // phase: read-only shares mount read-only
        {
            assert_eq!(
                mount_argv(&share("fs-1", "/h/a", true)),
                ["mount", "-t", "virtiofs", "-o", "ro", "fs-1", "/h/a"]
            );
            assert_eq!(
                mount_argv(&share("fs-1", "/h/a", false)),
                ["mount", "-t", "virtiofs", "fs-1", "/h/a"]
            );
        }
    }
}
