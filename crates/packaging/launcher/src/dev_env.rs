//! The launcher's developer-only environment overrides.
//!
//! A handful of environment variables re-point the launcher at trust or
//! policy inputs other than the image's: the system app store, the runtime
//! bundle root, the runtime trusted-publisher list, the JIT and
//! untrusted-parser policy files, the D-Bus filtering proxy binary, and the
//! switch that turns per-host egress filtering off. They are read through
//! [`package_store::dev_env::dev_override`], which a shipped build closes.

/// Names the runtime trusted-publisher list instead of the image's.
pub const TRUSTED_PUBLISHERS_FILE_ENV: &str = "TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE";
/// `off` downgrades a host-scoped network grant to unrestricted egress.
pub const NET_GATE_ENV: &str = "TILO_LAUNCHER_NET_GATE";
/// Names the `xdg-dbus-proxy` binary that filters an app's session bus.
pub const DBUS_PROXY_BIN_ENV: &str = "TILO_LAUNCHER_DBUS_PROXY_BIN";

/// Every variable the launcher (and the app host beside it) treats as a
/// developer-only override. The compositor removes these from a launcher's
/// environment when its own gate is closed.
pub const DEV_OVERRIDE_VARS: &[&str] = &[
    package_store::layout::SYSTEM_DIR_ENV,
    package_store::layout::SYSTEM_PAYLOAD_DIR_ENV,
    runtime_bundle::RUNTIME_DIR_ENV,
    TRUSTED_PUBLISHERS_FILE_ENV,
    crate::seccomp::JIT_POLICY_PATH_ENV,
    crate::seccomp::WEB_ENGINE_POLICY_PATH_ENV,
    crate::hardening::POLICY_PATH_ENV,
    NET_GATE_ENV,
    DBUS_PROXY_BIN_ENV,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_override_this_crate_reads_is_listed() {
        // Phase `every_override_this_crate_reads_is_listed`.
        // The list is what the compositor strips from a launcher's
        // environment, so an override read through `dev_override` but
        // missing here is honoured by a dev launcher even when the
        // compositor meant to remove it. Found by reading the sources:
        // every `dev_override(NAME)` whose `NAME` is a constant defined in
        // this crate must have that constant's value in the list.
        {
            let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
            let mut sources = String::new();
            let mut pending = vec![src];
            while let Some(dir) = pending.pop() {
                for entry in std::fs::read_dir(dir).expect("crate sources").flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        pending.push(path);
                    } else if path.extension().is_some_and(|e| e == "rs") {
                        sources.push_str(&std::fs::read_to_string(&path).expect("source"));
                        sources.push('\n');
                    }
                }
            }
            let consts: std::collections::HashMap<&str, &str> = sources
                .lines()
                .filter_map(|line| {
                    let rest = line.trim().strip_prefix("pub const ")?;
                    let (name, value) = rest.split_once(": &str = \"")?;
                    Some((name, value.strip_suffix("\";")?))
                })
                .collect();
            let mut read = Vec::new();
            for after in sources.split("dev_override(").skip(1) {
                let arg = after.split(')').next().expect("a closing paren");
                if let Some(name) = arg.rsplit("::").next()
                    && let Some(value) = consts.get(name)
                {
                    read.push((name, *value));
                }
            }
            assert!(
                read.iter()
                    .any(|(name, _)| *name == "WEB_ENGINE_POLICY_PATH_ENV"),
                "the scan must find the overrides the crate reads: {read:?}"
            );
            for (name, value) in read {
                assert!(
                    DEV_OVERRIDE_VARS.contains(&value),
                    "{name} ({value}) is read as a developer override but is not in \
                     DEV_OVERRIDE_VARS, so the compositor would not strip it"
                );
            }
        }
    }
}
