//! Fluent strings for the Windows compatibility manager.

ui::embedded_catalog!(pub(crate) static CATALOG, ["en-US"]);

/// Resolve a Windows Apps Fluent key.
pub fn t(key: &str) -> String {
    CATALOG.t(key)
}

/// Resolve `key` with one named variable (`{ $name }` in the message).
pub fn format(key: &str, name: &str, value: &str) -> String {
    CATALOG.t_args(key, &[(name, value.into())])
}

/// Resolve `key` with every named variable it takes.
pub fn format_all(key: &str, args: &[(&str, String)]) -> String {
    let vars: Vec<(&str, ui::i18n::ArgValue)> = args
        .iter()
        .map(|(name, value)| (*name, value.as_str().into()))
        .collect();
    CATALOG.t_args(key, &vars)
}

#[cfg(test)]
mod tests {
    use super::{format_all, t};

    #[test]
    fn controls_resolve_from_the_bundle() {
        for key in [
            "windows-route-wine",
            "windows-route-proton",
            "windows-route-vm",
            "windows-use-recommendation",
            "windows-choose-windows-iso",
            "windows-open-recovery-desktop",
            "windows-start-appliance",
            "windows-escalate-install-to-vm",
            "windows-restart-appliance",
            "windows-suspend-appliance",
            "windows-resume-appliance",
            "windows-shutdown-appliance",
            "windows-connect-for-activation",
        ] {
            assert_ne!(t(key), key, "missing Fluent key: {key}");
        }
    }

    /// The status and error sentences an appliance reports are the
    /// catalogue's too: nothing a person reads is a literal in the code.
    #[test]
    fn the_appliances_own_sentences_resolve_from_the_bundle() {
        for key in [
            "windows-power-running",
            "windows-power-suspended",
            "windows-power-paused",
            "windows-power-shut-down",
            "windows-power-unknown",
            "windows-phase-not-set-up",
            "windows-phase-installing",
            "windows-phase-finishing-setup",
            "windows-phase-ready",
            "windows-phase-needs-recovery",
            "windows-phase-unavailable",
            "windows-status-running",
            "windows-status-suspended",
            "windows-status-paused",
            "windows-status-shut-down",
            "windows-status-ready",
            "windows-status-not-set-up",
            "windows-status-installing",
            "windows-status-finishing-setup",
            "windows-status-needs-recovery",
            "windows-status-unavailable",
            "windows-license-unknown",
            "windows-license-digital",
            "windows-license-product-key",
            "windows-license-managed",
            "windows-license-evaluation",
            "windows-license-unlicensed",
            "windows-license-unknown-detail",
            "windows-license-unlicensed-detail",
            "windows-license-evaluation-detail",
            "windows-purpose-activation",
            "windows-purpose-update",
            "windows-connection-expired",
            "windows-connection-expires-seconds",
            "windows-connection-expires",
            "windows-app-status-ready",
            "windows-app-status-needs-attention",
            "windows-app-status-uninstalled",
            "windows-confidence-verified",
            "windows-confidence-community",
            "windows-confidence-best-match",
            "windows-confidence-untested",
            "windows-route-capsule-security",
            "windows-route-vm-security",
            "windows-error-unmanageable",
            "windows-error-busy",
            "windows-error-recovery-unavailable",
            "windows-error-connection-no-lease",
            "windows-media-accepted",
            "windows-recovery-open",
            "windows-connection-revoked",
        ] {
            assert_ne!(t(key), key, "missing Fluent key: {key}");
        }
        assert_eq!(
            format_all(
                "windows-connection-expires",
                &[("minutes", "9".to_owned()), ("seconds", "40".to_owned()),],
            ),
            "Expires in 9m 40s"
        );
    }
}
