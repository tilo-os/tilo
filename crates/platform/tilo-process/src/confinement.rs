//! Whether this process could build the user and mount namespace a confined guest needs:
//! not under a seccomp filter, with unprivileged user namespaces allowed.

/// Whether a seccomp filter confines this process.
#[must_use]
pub fn seccomp_filtered() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status.lines().find_map(|line| {
                line.strip_prefix("Seccomp:")
                    .map(|value| value.trim().to_owned())
            })
        })
        .and_then(|value| value.parse::<u32>().ok())
        .is_some_and(|mode| mode >= 2)
}

/// Whether the kernel allows unprivileged user namespaces. An absent sysctl means the kernel
/// default, so only an explicit disable reports false.
#[must_use]
pub fn unprivileged_userns_enabled() -> bool {
    // Debian/Ubuntu knob: `0` disables unprivileged user namespaces entirely.
    if let Ok(value) = std::fs::read_to_string("/proc/sys/kernel/unprivileged_userns_clone")
        && value.trim() == "0"
    {
        return false;
    }
    // Upstream cap on how many user namespaces may exist: `0` means none.
    if let Ok(value) = std::fs::read_to_string("/proc/sys/user/max_user_namespaces")
        && value.trim() == "0"
    {
        return false;
    }
    true
}
