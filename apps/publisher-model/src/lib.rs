//! The `publisher.profile` resource: the key that signs an app and the namespaces
//! the registry lets it publish under, as the app that draws it and an agent that
//! works it both see it.

use tilo_resource::{self, Model, Refusal, resource};

/// The registry the catalogue advertises: what a profile publishes to until the
/// person names another.
pub const DEFAULT_REGISTRY: &str = "https://apps.tilo.so";

/// What to apply for, and where. Each field is edited at its path; the registry must stay
/// an address the client accepts, however it is changed.
#[derive(Model, Debug, Clone, PartialEq)]
#[model(check = Publisher::valid, check_reads = "registry")]
pub struct Publisher {
    /// The registry the key publishes to.
    pub registry: String,
    /// A domain whose namespace to claim, such as `example.com`.
    pub domain: String,
    /// A forge account whose namespace to claim, such as `github:ada`.
    pub forge_account: String,
    /// How the registry's owner can reach you about the application.
    pub contact: String,
}

impl Default for Publisher {
    fn default() -> Self {
        Self {
            registry: DEFAULT_REGISTRY.to_owned(),
            domain: String::new(),
            forge_account: String::new(),
            contact: String::new(),
        }
    }
}

impl Publisher {
    /// What the resource holds: the registry the key publishes to, and the namespaces claimed
    /// on it.
    ///
    /// # Errors
    /// When a field cannot be read from the document.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }

    /// The registry is HTTPS or this machine.
    fn valid(&self) -> Result<(), Refusal> {
        registry_url(&self.registry).map_err(Refusal::new)
    }
}

#[resource(
    kind = "publisher.profile",
    read_content = Publisher::read_document
)]
impl Publisher {}

/// Why `base` is not a registry to publish to: credential-free `https`, or
/// plain `http` on this machine alone.
///
/// Mirrors `tilo_net::secure_endpoint` without pulling the network stack into
/// the portable model crate; the publisher app tests both against one address
/// table so the two cannot drift.
///
/// # Errors
///
/// When the address does not parse, carries credentials, or is reached in the
/// clear beyond this machine.
pub fn registry_url(base: &str) -> Result<(), String> {
    let url = url::Url::parse(base).map_err(|_| "not a registry address".to_owned())?;
    let secure_origin = match url.scheme() {
        "https" => true,
        "http" => match url.host() {
            Some(url::Host::Domain(name)) => {
                let name = name.trim_end_matches('.').to_ascii_lowercase();
                name == "localhost" || name.ends_with(".localhost")
            }
            Some(url::Host::Ipv4(address)) => address.is_loopback(),
            Some(url::Host::Ipv6(address)) => address.is_loopback(),
            None => false,
        },
        _ => false,
    };
    if !secure_origin
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(
            "a registry is reached over credential-free https (plain http only on this machine)"
                .to_owned(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_registry_is_https_or_this_machine() {
        for ok in [
            "https://apps.tilo.so",
            "http://localhost:8080",
            "http://localhost.:8080",
            "http://tilo.localhost:8080",
            "http://127.0.0.1:8080",
            "http://127.1.2.3:8080",
            "http://[::1]:8080",
        ] {
            assert!(registry_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://apps.tilo.so",
            "http://localhost.tilo.so",
            "http://0.0.0.0/",
            "http://[::]/",
            "http://[::ffff:127.0.0.1]/",
            "https://placeholder:placeholder@apps.tilo.so",
            "https://placeholder@apps.tilo.so",
            "https://:placeholder@apps.tilo.so",
            "http://placeholder:placeholder@[::1]:8080",
            "file:///tmp/registry",
            "not an address",
        ] {
            assert!(registry_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_new_profile_publishes_to_the_advertised_registry() {
        assert_eq!(Publisher::default().registry, DEFAULT_REGISTRY);
    }

    #[test]
    fn a_stored_profile_with_credentials_still_loads_until_edited() {
        let stored_url = "https://placeholder:placeholder@apps.example.com".to_owned();
        let mut stored = tilo_resource::Replica::<Publisher>::new();
        tilo_resource::write_key(
            stored.doc_mut(),
            &tilo_resource::__private::ROOT,
            "registry",
            &stored_url,
        )
        .unwrap();
        let saved = stored.save();

        let mut loaded = tilo_resource::Replica::<Publisher>::load(&saved).unwrap();
        assert_eq!(loaded.state().unwrap().registry, stored_url);

        // Editing another field still works; editing `registry` enforces the rule.
        loaded
            .apply(
                tilo_resource::Invoke::edit(&Publisher::paths().domain.replace("example.com"))
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(loaded.state().unwrap().domain, "example.com");
        assert!(
            loaded
                .apply(
                    tilo_resource::Invoke::edit(
                        &Publisher::paths()
                            .registry
                            .replace("https://placeholder:placeholder@other.example.com"),
                    )
                    .unwrap(),
                )
                .is_err()
        );
        loaded
            .apply(
                tilo_resource::Invoke::edit(
                    &Publisher::paths()
                        .registry
                        .replace("https://apps.example.com"),
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(loaded.state().unwrap().registry, "https://apps.example.com");
    }
}
