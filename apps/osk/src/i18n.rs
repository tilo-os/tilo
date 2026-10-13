//! The keyboard's Fluent catalogue, embedded so a `--shots` render, a unit
//! test and a live session resolve the same strings.

ui::embedded_catalog!(pub fn, ["en-US"]);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_resolves_to_a_real_string() {
        for key in [
            "osk-emoji-key",
            "osk-emoji-panel",
            "osk-emoji-search",
            "osk-emoji-recent",
            "osk-emoji-no-results",
            "osk-emoji-letters",
            "osk-emoji-letters-name",
            "osk-emoji-delete",
            "osk-emoji-end-search",
            "osk-emoji-tones-hint",
            "osk-emoji-tone-light",
            "osk-emoji-tone-medium-light",
            "osk-emoji-tone-medium",
            "osk-emoji-tone-medium-dark",
            "osk-emoji-tone-dark",
        ] {
            let s = t(key);
            assert_ne!(s, key, "{key} has no entry in the catalogue");
            assert!(!s.trim().is_empty(), "{key} resolves to nothing");
        }
        let insert = t_args("osk-emoji-insert", &[("name", "pizza".into())]);
        assert!(insert.contains("pizza"), "{insert}");
    }
}
