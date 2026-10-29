//! The launcher's Fluent catalogue, embedded so a capture, a test and a
//! live session resolve the same strings.

ui::embedded_catalog!(pub fn, ["en-US", "de-DE"]);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_resolves_to_a_real_string() {
        for key in [
            "launcher-placeholder",
            "launcher-filter-all",
            "launcher-filter-more",
            "launcher-a11y-filter-all",
            "launcher-a11y-filter-more",
            "launcher-a11y-filters",
            "launcher-a11y-result-kinds",
            "launcher-a11y-scope",
        ] {
            let s = t(key);
            assert_ne!(s, key, "{key} has no entry in the catalogue");
        }
        let chip = t_args("launcher-a11y-filter-kind", &[("kind", "Files".into())]);
        assert!(chip.contains("Files"), "{chip}");
    }
}
