//! The host's fluent catalogue: chrome only. What an app says comes from the app's own source.

ui::embedded_catalog!(pub fn, ["en-US", "de-DE"]);

/// Load the catalogue on this thread if it is not loaded yet.
pub fn ensure() {
    CATALOG.ensure();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_the_host_uses_resolves_to_a_real_string() {
        ensure();
        for key in [
            "tilolang-title",
            "tilolang-home-heading",
            "tilolang-home-empty",
            "tilolang-open-failed",
            "tilolang-back",
            "tilolang-detail",
            "tilolang-ask-title",
            "tilolang-ask-confirm",
            "tilolang-ask-decline",
            "tilolang-ask-always",
            "tilolang-ask-denied",
            "tilolang-ask-needs-approval",
            "tilolang-invocation-pending",
            "tilolang-invocation-done",
            "tilolang-invocation-suggested",
            "tilolang-invocation-declined",
            "tilolang-invocation-failed",
            "tilolang-invocation-denied",
            "tilolang-embed-error",
            "tilolang-dev-errors",
        ] {
            let text = t(key);
            assert_ne!(text, key, "{key} has no entry in the catalogue");
            assert!(!text.trim().is_empty(), "{key} resolves to nothing");
        }
    }
}
