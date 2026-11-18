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
#[model(check = Publisher::valid)]
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

/// Why `base` is not a registry to publish to: it is reached over https, or over
/// plain http on this machine alone.
///
/// # Errors
///
/// When the address does not parse or is reached in the clear beyond this machine.
pub fn registry_url(base: &str) -> Result<(), String> {
    let url = url::Url::parse(base).map_err(|_| "not a registry address".to_owned())?;
    let loopback = url.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err(
            "a registry is reached over https (plain http only on this machine)".to_owned(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_registry_is_https_or_this_machine() {
        assert!(registry_url("https://apps.tilo.so").is_ok());
        assert!(registry_url("http://localhost:8080").is_ok());
        assert!(registry_url("http://127.0.0.1:8080").is_ok());
        assert!(registry_url("http://[::1]:8080").is_ok());
        assert!(registry_url("http://apps.tilo.so").is_err());
        assert!(registry_url("not an address").is_err());
    }

    #[test]
    fn a_new_profile_publishes_to_the_advertised_registry() {
        assert_eq!(Publisher::default().registry, DEFAULT_REGISTRY);
    }
}
