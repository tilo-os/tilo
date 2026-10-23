//! Reverse-DNS package ids and the namespaces that own them.
//!
//! A package id is lowercase reverse DNS with at least three labels
//! (`com.example.notes`). A namespace is a shorter prefix of it
//! (`com.example`), and a publisher may claim an id only inside a namespace the
//! registry has verified for its key. Two kinds of proof exist, both documented
//! for publishers in `docs/publishing.md`:
//!
//! - a **domain** `example.com` gives `com.example`;
//! - a **forge account** gives `io.github.<account>`, `org.codeberg.<account>`,
//!   or the reversed host of another Forgejo or Gitea instance followed by the
//!   account (`org.example.git.<account>` for `git.example.org`).
//!
//! [`TILO_NAMESPACE`] belongs to Tilo itself.

pub const TILO_NAMESPACE: &str = "so.tilo";

/// Longest package id, in bytes.
pub const MAX_PACKAGE_ID_LEN: usize = 255;

/// One DNS label as ids use it: 1–63 of `a-z`, `0-9` and `-`, not starting or
/// ending with `-`.
#[must_use]
pub fn valid_label(label: &str) -> bool {
    (1..=63).contains(&label.len())
        && label
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !label.starts_with('-')
        && !label.ends_with('-')
}

fn labels_valid(name: &str, min_labels: usize) -> bool {
    name.len() <= MAX_PACKAGE_ID_LEN
        && name.split('.').count() >= min_labels
        && name.split('.').all(valid_label)
}

#[must_use]
pub fn valid_package_id(id: &str) -> bool {
    labels_valid(id, 3)
}

#[must_use]
pub fn valid_namespace(namespace: &str) -> bool {
    labels_valid(namespace, 2)
}

/// Whether `namespace` owns the package id `id`: `id` is `namespace` followed
/// by at least one more label.
#[must_use]
pub fn namespace_owns(namespace: &str, id: &str) -> bool {
    id.strip_prefix(namespace)
        .is_some_and(|rest| rest.len() > 1 && rest.starts_with('.'))
}

/// The namespace a domain proves: its labels reversed. `None` for a name that
/// is not a valid domain of at least two labels.
#[must_use]
pub fn domain_namespace(domain: &str) -> Option<String> {
    let domain = domain.trim_end_matches('.').to_ascii_lowercase();
    let namespace = domain.split('.').rev().collect::<Vec<_>>().join(".");
    valid_namespace(&namespace).then_some(namespace)
}

/// A git forge an account namespace can be proven on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Forge {
    /// `github.com`, namespace `io.github.<account>`.
    GitHub,
    /// `codeberg.org`, namespace `org.codeberg.<account>`.
    Codeberg,
    /// Another Forgejo or Gitea instance by host name, namespace
    /// `<reversed host>.<account>`.
    Forgejo(String),
}

impl Forge {
    /// Parse `github`, `codeberg`, or a host name.
    #[must_use]
    pub fn parse(forge: &str) -> Option<Self> {
        match forge.to_ascii_lowercase().as_str() {
            "github" | "github.com" => Some(Self::GitHub),
            "codeberg" | "codeberg.org" => Some(Self::Codeberg),
            host => domain_namespace(host).map(|_| Self::Forgejo(host.to_owned())),
        }
    }

    /// The forge's host name.
    #[must_use]
    pub fn host(&self) -> &str {
        match self {
            Self::GitHub => "github.com",
            Self::Codeberg => "codeberg.org",
            Self::Forgejo(host) => host,
        }
    }

    /// The namespace `account` on this forge proves. `None` when the account
    /// name is not a valid label (lowercased): such accounts prove by domain.
    #[must_use]
    pub fn namespace(&self, account: &str) -> Option<String> {
        let account = account.to_ascii_lowercase();
        if !valid_label(&account) {
            return None;
        }
        let prefix = match self {
            Self::GitHub => "io.github".to_owned(),
            Self::Codeberg => "org.codeberg".to_owned(),
            Self::Forgejo(host) => domain_namespace(host)?,
        };
        let namespace = format!("{prefix}.{account}");
        valid_namespace(&namespace).then_some(namespace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_ids_are_lowercase_reverse_dns_owned_by_a_prefix() {
        for id in [
            "so.tilo.notes",
            "com.example.notes",
            "io.github.ada.word-count",
        ] {
            assert!(valid_package_id(id), "{id}");
        }
        for id in [
            "so.tilo",
            "Com.example.notes",
            "com..notes",
            "com.example.-notes",
            "com.example.notes_2",
            "com.example.notes.",
        ] {
            assert!(!valid_package_id(id), "{id}");
        }
        assert!(!valid_package_id(&format!(
            "com.example.{}",
            "a".repeat(64)
        )));

        assert!(namespace_owns("com.example", "com.example.notes"));
        assert!(namespace_owns("com.example", "com.example.notes.cli"));
        assert!(!namespace_owns("com.example", "com.example"));
        assert!(!namespace_owns("com.example", "com.examples.notes"));
        assert!(!namespace_owns("com.example", "com.example."));
    }

    #[test]
    fn domains_and_forge_accounts_map_to_namespaces() {
        assert_eq!(
            domain_namespace("Example.COM.").as_deref(),
            Some("com.example")
        );
        assert_eq!(
            domain_namespace("apps.example.co.uk").as_deref(),
            Some("uk.co.example.apps")
        );
        assert_eq!(domain_namespace("localhost"), None);
        assert_eq!(domain_namespace("exa_mple.com"), None);

        assert_eq!(
            Forge::parse("github").unwrap().namespace("Ada").as_deref(),
            Some("io.github.ada")
        );
        assert_eq!(
            Forge::parse("codeberg.org")
                .unwrap()
                .namespace("ada")
                .as_deref(),
            Some("org.codeberg.ada")
        );
        let forgejo = Forge::parse("git.example.org").unwrap();
        assert_eq!(forgejo.host(), "git.example.org");
        assert_eq!(
            forgejo.namespace("ada").as_deref(),
            Some("org.example.git.ada")
        );
        assert_eq!(Forge::GitHub.namespace("ada_lovelace"), None);
        assert_eq!(Forge::parse("not a host"), None);
    }
}
