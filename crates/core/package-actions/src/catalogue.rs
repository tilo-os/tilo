//! What the workspace's Tilo repositories offer, as this device reads them for a caller that may
//! not (the Store, in its sandbox): a repository's verified catalogue, fresh or as this device
//! last accepted it, and the build of a runtime one offers. Reading a catalogue fresh moves the
//! device's anti-rollback checkpoint for that repository, which only the daemon may write; so
//! the readings run here, on the registries the workspace configures
//! ([`app_cli::config::load_registries`]), never on a list the caller hands over.
//!
//! A catalogue read the caller explains to a person (the catalogue expired, or the system is too
//! old to judge any repository) answers as a typed refusal, `{"refused": <reason>, "detail":
//! <text>}`: the reason decides what the caller says, and the detail is for logs. Anything else
//! that goes wrong fails the reading.

use serde_json::{Map, Value, json};

use app_cli::client::{RegistryError, VerifiedEntry};
use app_cli::config::{RegistriesFile, RegistryConfig};

/// The largest catalogue a reading answers.
pub const MAX_CATALOGUE_BYTES: usize = 4 * 1024 * 1024;

/// The largest runtime offer a reading answers.
const MAX_OFFER_BYTES: usize = 256 * 1024;

/// Why a catalogue read gave no rows, for the caller to say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The system's interface declaration cannot be read: the system needs an update before any
    /// repository can be judged.
    SystemUpdateNeeded,
    /// The repository's catalogue is authentic but expired.
    Expired,
    /// The repository serves a catalogue older than the one this device already accepted.
    BehindAccepted,
}

impl Refusal {
    /// The reason as a reading names it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::SystemUpdateNeeded => "system-update-needed",
            Self::Expired => "expired",
            Self::BehindAccepted => "behind-accepted",
        }
    }
}

/// A catalogue read that gave no rows: refused for a reason the caller explains, or failed.
#[derive(Debug)]
enum ReadError {
    Refused(Refusal, String),
    Failed(String),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(refusal, detail) => write!(f, "{}: {detail}", refusal.name()),
            Self::Failed(error) => f.write_str(error),
        }
    }
}

/// Answer the reading `id` with `arguments`, when it is one of this module's: `catalogue`
/// (`{registry, fresh}`) or `runtime-offer` (`{registry, runtime}`).
#[must_use]
pub fn reading(id: &str, arguments: &Map<String, Value>) -> Option<Result<Value, String>> {
    let text = |name: &str| {
        arguments
            .get(name)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("`{name}` is required"))
    };
    let answer = match id {
        "catalogue" => text("registry").and_then(|registry| {
            let fresh = arguments
                .get("fresh")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            catalogue_answer(catalogue(registry, fresh))
        }),
        "runtime-offer" => text("registry").and_then(|registry| {
            text("runtime").and_then(|runtime| runtime_offer(registry, runtime))
        }),
        _ => return None,
    };
    Some(answer)
}

/// `registry`'s verified catalogue: asked of the repository now when `fresh`, else the one this
/// device last accepted from it, read without the network.
fn catalogue(registry: &str, fresh: bool) -> Result<Vec<VerifiedEntry>, ReadError> {
    system_declaration_readable(&package_store::interfaces::system_interfaces_path())?;
    let configured = app_cli::config::load_registries().map_err(ReadError::Failed)?;
    if fresh {
        catalogue_from(&configured, registry, app_cli::client::fetch_entries)
    } else {
        catalogue_from(
            &configured,
            registry,
            app_cli::client::last_verified_entries,
        )
    }
}

/// The reading's answer: the entries, or the typed refusal; anything else fails it.
fn catalogue_answer(read: Result<Vec<VerifiedEntry>, ReadError>) -> Result<Value, String> {
    match read {
        Ok(entries) => {
            let entries = serde_json::to_value(entries).map_err(|error| error.to_string())?;
            Ok(json!({ "entries": entries }))
        }
        Err(ReadError::Refused(refusal, detail)) => {
            Ok(json!({ "refused": refusal.name(), "detail": detail }))
        }
        Err(ReadError::Failed(error)) => Err(error),
    }
}

/// Refuse to read any repository while the system's interface declaration at `path` cannot be
/// read ([`Refusal::SystemUpdateNeeded`]).
///
/// Which builds of an app run here is decided against that declaration. Unreadable, it was written
/// for another version of the system than this daemon, so the caller must say the system needs an
/// update. A device without a declaration (a development machine) judges nothing and reads
/// repositories as they are.
fn system_declaration_readable(path: &std::path::Path) -> Result<(), ReadError> {
    match package_store::interfaces::read_interfaces(path) {
        Ok(_) => Ok(()),
        Err(error) => {
            tracing::error!(%error, "the system's interface declaration cannot be read; repositories are refused until the system is updated");
            Err(ReadError::Refused(
                Refusal::SystemUpdateNeeded,
                error.to_string(),
            ))
        }
    }
}

/// The repository `name` among `configured`, read with `fetch`, when it is configured and enabled.
fn catalogue_from(
    configured: &RegistriesFile,
    name: &str,
    fetch: impl FnOnce(&RegistryConfig) -> Result<Vec<VerifiedEntry>, RegistryError>,
) -> Result<Vec<VerifiedEntry>, ReadError> {
    let registry = enabled(configured, name).map_err(ReadError::Failed)?;
    let entries = fetch(registry).map_err(|error| match error {
        RegistryError::Expired { .. } => ReadError::Refused(Refusal::Expired, error.to_string()),
        RegistryError::BehindAccepted { .. } => {
            ReadError::Refused(Refusal::BehindAccepted, error.to_string())
        }
        other => ReadError::Failed(other.to_string()),
    })?;
    let size = serde_json::to_vec(&entries)
        .map_err(|error| ReadError::Failed(error.to_string()))?
        .len();
    if size > MAX_CATALOGUE_BYTES {
        return Err(ReadError::Failed(
            "the verified repository catalogue exceeds the response limit".into(),
        ));
    }
    Ok(entries)
}

/// The enabled repository `name` among `configured`.
fn enabled<'a>(configured: &'a RegistriesFile, name: &str) -> Result<&'a RegistryConfig, String> {
    if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
        return Err("invalid repository name".into());
    }
    configured
        .registries
        .iter()
        .find(|registry| registry.name == name && registry.enabled)
        .ok_or_else(|| "the repository is not configured and enabled".into())
}

/// The build `runtime` that the enabled repository `registry` offers for this device:
/// `{"offer": <offer>}`, the offer `null` when it offers none.
fn runtime_offer(registry: &str, runtime: &str) -> Result<Value, String> {
    if runtime.is_empty() || runtime.len() > 256 || runtime.chars().any(char::is_control) {
        return Err("invalid runtime".into());
    }
    enabled(&app_cli::config::load_registries()?, registry)?;
    let offer = app_cli::runtime_offer(runtime, registry)?;
    let answer = json!({ "offer": offer });
    if answer.to_string().len() > MAX_OFFER_BYTES {
        return Err("the runtime offer exceeds the response limit".into());
    }
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configuration() -> RegistriesFile {
        RegistriesFile {
            registries: vec![RegistryConfig {
                name: "Configured source".into(),
                base_url: "https://registry.example.test/".into(),
                owner_pubkey_hex: "17".repeat(32),
                enabled: true,
                channel: tilo_app_package::version::ReleaseChannel::Stable,
            }],
        }
    }

    /// A declaration the daemon cannot read refuses every repository with the reason the caller
    /// shows, never an empty or silently unjudged catalogue; a readable one, or none, reads.
    #[test]
    fn an_unreadable_system_declaration_refuses_saying_the_system_needs_an_update() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("system-interfaces.toml");
        std::fs::write(&path, "[interfaces]\nsystem = \"one\"\n").unwrap();
        let refused = system_declaration_readable(&path).unwrap_err();
        assert!(
            matches!(&refused, ReadError::Refused(Refusal::SystemUpdateNeeded, detail) if detail.contains("system-interfaces.toml")),
            "{refused}"
        );
        assert_eq!(
            catalogue_answer(Err(refused)).unwrap()["refused"],
            "system-update-needed"
        );

        std::fs::write(
            &path,
            "[interfaces]\nsystem = [1]\nshell-ui = [11]\nruntime = [2]\n",
        )
        .unwrap();
        assert!(system_declaration_readable(&path).is_ok());
        std::fs::remove_file(&path).unwrap();
        assert!(system_declaration_readable(&path).is_ok());
    }

    /// Only a configured, enabled repository is ever fetched, by its configured address and key.
    #[test]
    fn only_an_enabled_configured_repository_is_read() {
        let mut configured = configuration();
        let entries = catalogue_from(&configured, "Configured source", |registry| {
            assert_eq!(registry.base_url, "https://registry.example.test/");
            assert_eq!(registry.owner_pubkey_hex, "17".repeat(32));
            Ok(Vec::new())
        })
        .unwrap();
        assert!(entries.is_empty());
        assert_eq!(
            catalogue_answer(Ok(entries)).unwrap(),
            json!({ "entries": [] })
        );
        for name in ["", "https://attacker.example.test/", "Configured source\n"] {
            assert!(catalogue_from(&configured, name, |_| panic!("unconfigured fetch")).is_err());
        }
        configured.registries[0].enabled = false;
        assert!(
            catalogue_from(&configured, "Configured source", |_| panic!(
                "disabled fetch"
            ))
            .is_err()
        );
    }

    /// An expired catalogue answers as a typed refusal; an unreachable repository fails the
    /// reading.
    #[test]
    fn an_expired_or_behind_catalogue_is_refused_typed() {
        let configured = configuration();
        let expired = catalogue_from(&configured, "Configured source", |_| {
            Err(RegistryError::Expired {
                expired_at_unix: 1_792_587_897,
            })
        })
        .unwrap_err();
        assert!(
            matches!(expired, ReadError::Refused(Refusal::Expired, _)),
            "{expired}"
        );
        let answer = catalogue_answer(Err(expired)).unwrap();
        assert_eq!(answer["refused"], "expired");
        assert!(answer["detail"].as_str().unwrap().contains("1792587897"));

        let offline = catalogue_from(&configured, "Configured source", |_| {
            Err(RegistryError::Http("connection refused".into()))
        })
        .unwrap_err();
        assert!(matches!(offline, ReadError::Failed(_)), "{offline}");
        assert!(
            catalogue_answer(Err(offline))
                .unwrap_err()
                .contains("connection refused")
        );
    }

    /// A rolled-back catalogue fails with its reason, and one larger than a reading answers is
    /// refused.
    #[test]
    fn a_rolled_back_or_oversized_catalogue_fails() {
        let configured = configuration();
        let error = catalogue_from(&configured, "Configured source", |_| {
            Err(RegistryError::Rollback)
        })
        .unwrap_err();
        assert!(error.to_string().contains("rolled back or replayed"));
        let entry: VerifiedEntry = serde_json::from_value(json!({
            "entry": {
                "package_id": "so.tilo.catalogue-test", "kind": "app", "name": {"en": "Catalogue test"},
                "description": {"en": "x".repeat(MAX_CATALOGUE_BYTES)},
                "tagline": {"en": ""}, "images": {}, "supported_languages": [], "categories": [],
                "source_url": null, "homepage": null, "publisher_pubkey_hex": "17".repeat(32),
                "versions": [], "updated_at_unix": 0, "signature_hex": ""
            }, "trust": "PublisherSigned", "version_index": null
        }))
        .unwrap();
        let error =
            catalogue_from(&configured, "Configured source", |_| Ok(vec![entry])).unwrap_err();
        assert!(error.to_string().contains("exceeds the response limit"));
    }

    /// A reading this module does not serve is left to the others; one it serves names what it
    /// lacks.
    #[test]
    fn a_reading_names_the_argument_it_lacks() {
        assert!(reading("flatpak", &Map::new()).is_none());
        let refused = reading("catalogue", &Map::new()).unwrap().unwrap_err();
        assert!(refused.contains("registry"), "{refused}");
        let mut arguments = Map::new();
        arguments.insert("registry".into(), Value::String("main".into()));
        let refused = reading("runtime-offer", &arguments).unwrap().unwrap_err();
        assert!(refused.contains("runtime"), "{refused}");
    }
}
