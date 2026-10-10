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
    crate::hardening::POLICY_PATH_ENV,
    NET_GATE_ENV,
    DBUS_PROXY_BIN_ENV,
];
