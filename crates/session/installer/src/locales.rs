//! Locale choices supported end-to-end by the Imager, live installer and OS.

const CATALOG: &str = include_str!("../../../../os/usr/share/tilo/installer-locales.txt");

pub fn list() -> Vec<String> {
    values().map(str::to_string).collect()
}

pub fn contains(candidate: &str) -> bool {
    values().any(|locale| locale == candidate)
}

fn values() -> impl Iterator<Item = &'static str> {
    CATALOG
        .lines()
        .map(str::trim)
        .filter(|locale| !locale.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_unique_safe_and_contains_the_default() {
        let locales = list();
        assert!(locales.iter().any(|locale| locale == "en_US.UTF-8"));
        assert!(locales.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(locales.iter().all(|locale| {
            locale
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
        }));
    }
}
