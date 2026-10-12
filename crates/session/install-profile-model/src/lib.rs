//! The secret-free setup profile the Imager writes onto installation media
//! and the live installer reads back. Both sides check each field with these
//! rules, so the Imager refuses exactly what the installer would ask to
//! repair.

/// Purpose changes defaults, never the OS image or collaboration authority.
pub const PURPOSES: [&str; 2] = ["computer", "server"];

#[must_use]
pub fn valid_purpose(purpose: &str) -> bool {
    PURPOSES.contains(&purpose)
}

pub fn default_purpose() -> String {
    "computer".into()
}

/// Public binding to an opaque encrypted setup package, never its contents.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, try_from = "RawPreparedSetupExpectation")]
pub struct PreparedSetupExpectation {
    pub schema: u32,
    pub sealed_bytes: u64,
    pub ciphertext_sha256: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPreparedSetupExpectation {
    schema: u32,
    sealed_bytes: u64,
    ciphertext_sha256: String,
}

impl TryFrom<RawPreparedSetupExpectation> for PreparedSetupExpectation {
    type Error = &'static str;
    fn try_from(raw: RawPreparedSetupExpectation) -> Result<Self, Self::Error> {
        let value = Self {
            schema: raw.schema,
            sealed_bytes: raw.sealed_bytes,
            ciphertext_sha256: raw.ciphertext_sha256,
        };
        value
            .valid()
            .then_some(value)
            .ok_or("invalid prepared setup expectation")
    }
}

impl PreparedSetupExpectation {
    #[must_use]
    pub fn valid(&self) -> bool {
        self.schema == 1
            && (48..=32 * 1024 * 1024 + 16384).contains(&self.sealed_bytes)
            && self.ciphertext_sha256.len() == 64
            && self
                .ciphertext_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    }
}

/// Empty means no friendly label; supplied labels are bounded UTF-8 text.
#[must_use]
pub fn valid_device_label(label: &str) -> bool {
    (label.is_empty() || !label.trim().is_empty())
        && label.len() <= 64
        && !label.chars().any(char::is_control)
}

/// Locale choices supported end to end by the Imager, live installer and OS.
const LOCALE_CATALOG: &str = include_str!("../locales.txt");

/// Keyboard layouts the installer offers, by XKB id.
pub const KEYBOARDS: &[&str] = &[
    "us", "gb", "de", "fr", "es", "it", "pt", "br", "ru", "se", "no", "dk", "ch", "pl", "dvorak",
    "colemak",
];

/// Filesystems the installer formats the target with.
pub const FILESYSTEMS: [&str; 3] = ["btrfs", "ext4", "xfs"];

/// Accounts the OS image already owns.
const RESERVED_USERNAMES: &[&str] = &[
    "root", "bin", "daemon", "adm", "lp", "sync", "shutdown", "halt", "mail", "operator", "games",
    "ftp", "nobody", "dbus", "polkitd", "sshd", "chrony", "tss", "greetd", "tilo",
];

/// Every supported locale, in catalog order.
pub fn locales() -> impl Iterator<Item = &'static str> {
    LOCALE_CATALOG
        .lines()
        .map(str::trim)
        .filter(|locale| !locale.is_empty())
}

#[must_use]
pub fn valid_locale(locale: &str) -> bool {
    locales().any(|supported| supported == locale)
}

/// 1–63 letters, digits or hyphens, without a hyphen at either end: one DNS
/// label, which is also what `<hostname>.local` needs. Surrounding
/// whitespace is ignored.
#[must_use]
pub fn valid_hostname(hostname: &str) -> bool {
    let hostname = hostname.trim();
    !hostname.is_empty()
        && hostname.len() <= 63
        && hostname
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !hostname.starts_with('-')
        && !hostname.ends_with('-')
}

/// A non-reserved account name portable across the Linux `useradd`
/// implementations the installer supports.
#[must_use]
pub fn valid_username(username: &str) -> bool {
    let bytes = username.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 32
        && bytes
            .first()
            .is_some_and(|c| matches!(c, b'a'..=b'z' | b'_'))
        && bytes
            .iter()
            .skip(1)
            .all(|c| matches!(c, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
        && !RESERVED_USERNAMES.contains(&username)
}

/// At most 100 characters, none of them control characters or the colon
/// `passwd` separates fields with.
#[must_use]
pub fn valid_full_name(full_name: &str) -> bool {
    full_name.chars().count() <= 100
        && !full_name
            .chars()
            .any(|c| c.is_control() || matches!(c, ':' | '\n' | '\r'))
}

/// A relative IANA zone name that cannot climb out of the zoneinfo tree.
#[must_use]
pub fn valid_timezone_name(timezone: &str) -> bool {
    !timezone.is_empty()
        && timezone.len() <= 128
        && !timezone.starts_with('/')
        && !timezone.contains("..")
        && timezone
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'/' | b'+' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purpose_selects_only_defaults_for_one_os() {
        assert_eq!(default_purpose(), "computer");
        assert!(valid_purpose("computer"));
        assert!(valid_purpose("server"));
        assert!(!valid_purpose("desktop-os"));
        assert!(!valid_purpose("server\n"));
    }

    #[test]
    fn locale_catalog_is_unique_safe_and_contains_the_default() {
        let locales: Vec<_> = locales().collect();
        assert!(locales.contains(&"en_US.UTF-8"));
        assert!(locales.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(locales.iter().all(|locale| {
            locale
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
        }));
    }
}
