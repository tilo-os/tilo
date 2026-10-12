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

#[cfg(test)]
mod tests {
    use super::t;
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
}
