//! Whether this computer lets a process make a user namespace: what every app's sandbox is built
//! in. A computer that allows none cannot start any app, and the launcher says so by its exit code,
//! so whoever started it can name the cause.

/// The exit code of a launch whose sandbox could not be made because this computer allows no user
/// namespaces. Next to [`crate::services::EXIT_REQUIRED_SERVICE_MISSING`]: a cause the launcher
/// knows, which the session names in the person's words.
pub const EXIT_NO_USER_NAMESPACES: i32 = 71;

/// A kernel setting that turns user namespaces off: where it is read, the value that turns them
/// off, and the value that turns them on again.
#[cfg(target_os = "linux")]
struct Setting {
    name: &'static str,
    off: &'static str,
    on: &'static str,
}

/// The settings that turn user namespaces off, each distribution's own: Debian's switch, the limit
/// every kernel has, and Ubuntu's `AppArmor` restriction, which lets the namespace be made and then
/// takes away what a sandbox needs in it.
#[cfg(target_os = "linux")]
const SETTINGS: [Setting; 3] = [
    Setting {
        name: "kernel.unprivileged_userns_clone",
        off: "0",
        on: "1",
    },
    Setting {
        name: "user.max_user_namespaces",
        off: "0",
        on: "15000",
    },
    Setting {
        name: "kernel.apparmor_restrict_unprivileged_userns",
        off: "1",
        on: "0",
    },
];

/// Why this computer makes no sandbox for an app: the setting found turning user namespaces off,
/// or a refusal no setting here explains (a container's or a security module's).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The setting `name` turns them off; `sysctl name=on` turns them on until the next start.
    Setting {
        name: &'static str,
        on: &'static str,
    },
    /// Making one is refused, and no setting here says why.
    Policy,
}

/// Said one fact to a line, the cause first. A line to type or paste is indented four spaces, so
/// whoever shows it can set it apart and offer to copy it.
impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Setting { name, on } => write!(
                f,
                "{name} turns off user namespaces, which every app's sandbox is made in.\n\
                 Turn them on:\n    \
                 sudo sysctl {name}={on}\n\
                 Keep them on after a restart with this line in a file under /etc/sysctl.d/:\n    \
                 {name} = {on}"
            ),
            Self::Policy => f.write_str(
                "A container or security policy refuses user namespaces, which every app's \
                 sandbox is made in. No kernel setting here turns them off.\n\
                 Allow user namespaces where this computer runs.",
            ),
        }
    }
}

/// The first of [`SETTINGS`] that `read` finds turning user namespaces off.
#[cfg(target_os = "linux")]
fn setting_off(read: impl Fn(&str) -> Option<String>) -> Option<Refusal> {
    SETTINGS.iter().find_map(|setting| {
        (read(setting.name)?.trim() == setting.off).then_some(Refusal::Setting {
            name: setting.name,
            on: setting.on,
        })
    })
}

/// Why a process started here may not make a user namespace of its own, or `None` when it may.
/// A setting that turns them off is named first; otherwise a child tries to make one, so a
/// namespace limit and a container's filter answer too.
///
/// # Errors
///
/// The child could not be started to try: the answer is unknown.
#[cfg(target_os = "linux")]
pub fn refusal() -> std::io::Result<Option<Refusal>> {
    use std::os::unix::process::CommandExt as _;
    if let Some(off) = setting_off(|name| {
        std::fs::read_to_string(format!("/proc/sys/{}", name.replace('.', "/"))).ok()
    }) {
        return Ok(Some(off));
    }
    let mut probe = std::process::Command::new("/usr/bin/true");
    // SAFETY: the closure runs in the forked child before exec and only makes one syscall.
    unsafe {
        probe.pre_exec(|| {
            if libc::unshare(libc::CLONE_NEWUSER) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    match probe.status() {
        Ok(status) if status.success() => Ok(None),
        Ok(_) => Ok(Some(Refusal::Policy)),
        Err(error)
            if matches!(
                error.raw_os_error(),
                Some(libc::EPERM | libc::ENOSPC | libc::EUSERS | libc::EINVAL)
            ) =>
        {
            Ok(Some(Refusal::Policy))
        }
        Err(error) => Err(error),
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::{Refusal, setting_off};

    /// The settings of a computer, as `/proc/sys` reads them.
    fn computer<'a>(settings: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            settings
                .iter()
                .find(|(set, _)| *set == name)
                .map(|(_, value)| format!("{value}\n"))
        }
    }

    /// Debian with its switch off, Ubuntu with its `AppArmor` restriction on, and a kernel with no
    /// namespaces left to give are each named with the step that turns that one setting back on;
    /// a computer whose settings allow them names none.
    #[test]
    fn the_setting_found_off_is_named_with_the_step_that_turns_it_on() {
        let debian = computer(&[
            ("kernel.unprivileged_userns_clone", "0"),
            ("user.max_user_namespaces", "63000"),
        ]);
        assert_eq!(
            setting_off(debian),
            Some(Refusal::Setting {
                name: "kernel.unprivileged_userns_clone",
                on: "1"
            })
        );
        let ubuntu = computer(&[
            ("user.max_user_namespaces", "63000"),
            ("kernel.apparmor_restrict_unprivileged_userns", "1"),
        ]);
        let refusal = setting_off(ubuntu).expect("the restriction is named");
        let said = refusal.to_string();
        let lines: Vec<_> = said.lines().collect();
        assert!(
            lines[0].starts_with("kernel.apparmor_restrict_unprivileged_userns turns off"),
            "the cause comes first: {said}"
        );
        assert!(
            lines.contains(&"    sudo sysctl kernel.apparmor_restrict_unprivileged_userns=0"),
            "the command is a line of its own, to copy: {said}"
        );
        assert!(
            lines.contains(&"    kernel.apparmor_restrict_unprivileged_userns = 0"),
            "so is the line for /etc/sysctl.d/: {said}"
        );
        let none_left = computer(&[("user.max_user_namespaces", "0")]);
        assert_eq!(
            setting_off(none_left),
            Some(Refusal::Setting {
                name: "user.max_user_namespaces",
                on: "15000"
            })
        );
        let fedora = computer(&[
            ("user.max_user_namespaces", "63000"),
            ("kernel.apparmor_restrict_unprivileged_userns", "0"),
        ]);
        assert_eq!(setting_off(fedora), None);
    }
}
