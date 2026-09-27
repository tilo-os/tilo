//! Provider registry for `format-providers.toml`.

use std::path::{Path, PathBuf};

use format_provider_abi::Tier;
use serde::Deserialize;

use crate::runner::Limits;

/// One provider as declared in the registry TOML.
#[derive(Clone, Debug, PartialEq)]
pub struct ProviderEntry {
    /// MIME type the provider claims (e.g. `text/markdown`).
    pub mime: String,
    /// Package identifier (e.g. `so.tilo.qdoc-md`). Also the provider id used
    /// by clients in `importAs`/`exportAs`.
    pub package: String,
    /// Path to the provider's core WebAssembly module.
    pub wasm: PathBuf,
    pub version: String,
    /// Trust tier. Determines priority when multiple providers claim the same
    /// MIME — `Builtin` > `Installed` > `Ephemeral`.
    pub tier: Tier,
    /// Trust roots the host considers when admitting the provider. Free-form
    /// strings consumed by the package layer; the host stores them verbatim.
    pub trust: Vec<String>,
}

#[derive(Deserialize, Debug)]
struct WireRegistry {
    #[serde(default, alias = "providers", alias = "provider")]
    provider: Vec<WireProvider>,
    #[serde(default)]
    limits: WireLimits,
}

/// Optional `[limits]` table overriding the per-call budget for every provider.
#[derive(Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
struct WireLimits {
    fuel: Option<u64>,
    max_memory_mib: Option<usize>,
}

#[derive(Deserialize, Debug)]
struct WireProvider {
    mime: String,
    package: String,
    wasm: PathBuf,
    #[serde(default)]
    version: String,
    /// In the TOML this field is called `priority` (matches the on-disk
    /// example); the host maps it to the [`Tier`] enum on load.
    priority: String,
    #[serde(default)]
    trust: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Registry {
    pub providers: Vec<ProviderEntry>,
    /// Per-call resource budget applied to every provider.
    pub limits: Limits,
}

impl Registry {
    /// Load from a TOML file. A missing file is treated as an empty registry —
    /// the host stays up with no providers and clients get clear
    /// "unsupported mime" errors.
    ///
    /// # Errors
    ///
    /// Returns an error if reading or parsing the file fails (except for missing files).
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.into()),
        };
        Self::parse(&raw)
    }

    /// Parse the TOML body.
    ///
    /// # Errors
    ///
    /// Returns an error if parsing the TOML fails, if a provider declares
    /// an unknown priority tier, or if `[limits]` is out of range.
    pub fn parse(toml_text: &str) -> anyhow::Result<Self> {
        let wire: WireRegistry = toml::from_str(toml_text)?;
        let mut providers = Vec::with_capacity(wire.provider.len());
        for p in wire.provider {
            let tier = Tier::parse(&p.priority).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown priority {:?} for provider {}: expected one of builtin|installed|ephemeral",
                    p.priority,
                    p.package
                )
            })?;
            providers.push(ProviderEntry {
                mime: p.mime,
                package: p.package,
                wasm: p.wasm,
                version: p.version,
                tier,
                trust: p.trust,
            });
        }
        let defaults = Limits::default();
        let max_memory_bytes = match wire.limits.max_memory_mib {
            Some(mib) => mib
                .checked_mul(1024 * 1024)
                .filter(|b| *b > 0)
                .ok_or_else(|| anyhow::anyhow!("limits.max_memory_mib {mib} is out of range"))?,
            None => defaults.max_memory_bytes,
        };
        let limits = Limits {
            max_memory_bytes,
            fuel: wire.limits.fuel.unwrap_or(defaults.fuel),
        };
        Ok(Self { providers, limits })
    }

    /// Lookup a provider by its package id (the client-facing handle).
    pub fn by_package(&self, id: &str) -> Option<&ProviderEntry> {
        self.providers.iter().find(|p| p.package == id)
    }

    /// Highest-tier provider claiming a MIME type, with ties broken by file
    /// order (which is also the priority resolution policy documented in the
    /// host's open-questions section).
    pub fn best_for_mime(&self, mime: &str) -> Option<&ProviderEntry> {
        self.providers
            .iter()
            .filter(|p| p.mime == mime)
            .max_by_key(|p| p.tier)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One `[[provider]]` table.
    fn provider(mime: &str, package: &str, wasm: &str, priority: &str, version: &str) -> String {
        format!(
            "[[provider]]\nmime = {mime:?}\npackage = {package:?}\nwasm = {wasm:?}\n\
             version = {version:?}\npriority = {priority:?}\n"
        )
    }

    #[test]
    fn registry_parses_providers_and_limits_and_picks_the_highest_tier() {
        // A well-formed provider.
        let toml = provider(
            "text/markdown",
            "so.tilo.qdoc-md",
            "/path/to/provider.wasm",
            "builtin",
            "1.0.0",
        ) + "trust = [\"tilo-curated/2025\"]\n";
        let r = Registry::parse(&toml).unwrap();
        assert_eq!(r.providers.len(), 1);
        let p = &r.providers[0];
        assert_eq!(p.mime, "text/markdown");
        assert_eq!(p.package, "so.tilo.qdoc-md");
        assert_eq!(p.tier, Tier::Builtin);
        assert_eq!(p.trust, vec!["tilo-curated/2025".to_string()]);

        // An unknown priority is refused.
        let weird = provider(
            "text/markdown",
            "so.tilo.qdoc-md",
            "/x.wasm",
            "weird",
            "1.0.0",
        );
        assert!(Registry::parse(&weird).is_err());

        // Limits default, override, and refuse a zero cap or an unknown key.
        assert_eq!(Registry::parse("").unwrap().limits, Limits::default());
        let r = Registry::parse("[limits]\nfuel = 1000\nmax_memory_mib = 64\n").unwrap();
        assert_eq!(
            r.limits,
            Limits {
                max_memory_bytes: 64 * 1024 * 1024,
                fuel: 1000,
            }
        );
        assert!(Registry::parse("[limits]\nmax_memory_mib = 0\n").is_err());
        assert!(Registry::parse("[limits]\nwallclock = 3\n").is_err());

        // A missing file is an empty registry.
        let r = Registry::load(Path::new("/nonexistent/tilo-format-host/test.toml")).unwrap();
        assert!(r.providers.is_empty());

        // The highest tier wins for a mime; an unknown mime has no provider.
        let toml = provider("text/markdown", "low", "/a.wasm", "ephemeral", "0")
            + "\n"
            + &provider("text/markdown", "high", "/b.wasm", "builtin", "0");
        let r = Registry::parse(&toml).unwrap();
        assert_eq!(r.best_for_mime("text/markdown").unwrap().package, "high");
        assert!(r.best_for_mime("application/x-nope").is_none());
    }
}
