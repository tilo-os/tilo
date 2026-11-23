//! Shared provider administration catalogues.
ui::embedded_catalog!(pub fn, ["en-US", "de-DE"]);

#[cfg(test)]
mod tests {
    use super::t;
    use std::collections::BTreeMap;

    fn messages(source: &str) -> BTreeMap<&str, &str> {
        source
            .lines()
            .filter(|line| !line.starts_with('#') && !line.starts_with(' '))
            .filter_map(|line| line.split_once(" = "))
            .collect()
    }

    fn variables(pattern: &str) -> Vec<&str> {
        let mut vars: Vec<_> = pattern
            .split("{ $")
            .skip(1)
            .filter_map(|rest| rest.split_once(' ').map(|(name, _)| name))
            .collect();
        vars.sort_unstable();
        vars
    }

    #[test]
    fn shared_catalogues_translate_the_same_keys_and_variables() {
        let en = messages(include_str!("../locales/en-US.ftl"));
        let de = messages(include_str!("../locales/de-DE.ftl"));
        assert!(!en.is_empty());
        assert_eq!(en.keys().collect::<Vec<_>>(), de.keys().collect::<Vec<_>>());
        for (key, pattern) in en {
            assert_eq!(variables(pattern), variables(de[key]), "{key}");
            assert!(!de[key].trim().is_empty(), "{key}");
            assert_ne!(t(key), key);
            assert!(!t(key).trim().is_empty());
        }
    }
}
