//! Public Agent conversation status uses the shared Fluent manager.
use ui::i18n::EmbeddedCatalog;

static CATALOG: EmbeddedCatalog = EmbeddedCatalog::new(
    "en-US",
    &[("en-US", include_str!("../locales/en-US.ftl"))],
    "agent-message-actions-not-run",
)
.with_system_locale();

pub(crate) fn t(key: &str) -> String {
    CATALOG.t(key)
}

#[cfg(test)]
mod tests {
    #[test]
    fn conversation_status_keys_resolve() {
        for key in [
            "agent-message-actions-not-run",
            "agent-message-no-reply",
            "agent-message-model-unavailable",
            "agent-message-reply-too-long",
        ] {
            let value = super::t(key);
            assert!(!value.trim().is_empty());
            assert_ne!(value, key);
        }
    }
}
