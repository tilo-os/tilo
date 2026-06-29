//! TOML configuration schema for `qos-relay`.
//!
//! `RelayConfig::default()` is what ships when no TOML file is provided.
//! All fields are also overrideable on the CLI via `qos-relay --listen
//! /ip4/.../tcp/4001 --bootstrap /ip4/.../tcp/4001/p2p/<id> --log debug`.
//!
//! The loader is intentionally tolerant of missing fields — a config
//! file that only sets `bootstrap` should not have to repeat the
//! default `listen` values. `serde(default)` per field handles that.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use libp2p::Multiaddr;
use serde::{Deserialize, Serialize};

/// On-disk config for a relay instance.
///
/// Every field has a `serde(default)` so a sparse TOML file inherits
/// the rest of the defaults. The `Default` impl is the source of
/// truth for "what does an out-of-the-box relay do".
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct RelayConfig {
    /// Multiaddrs the relay listens on. Each address is dialled
    /// through `Swarm::listen_on`. Defaults to dual-stack IPv4
    /// `0.0.0.0` on TCP and QUIC port 4001.
    #[serde(default = "default_listen")]
    pub listen: Vec<Multiaddr>,

    /// Persistent keypair path. Generated on first run if missing.
    /// The CLI's `--identity` flag (when added) overrides this.
    /// Default: `/var/lib/qos-relay/identity.key` — matches the
    /// systemd unit's `StateDirectory=qos-relay`.
    #[serde(default = "default_identity_path")]
    pub identity_path: PathBuf,

    /// Bootstrap multiaddrs to dial on startup. These seed the
    /// Kademlia routing table; an empty list means "I am the first
    /// relay" (rendezvous will be slow until peers connect via
    /// mdns or `--bootstrap` from the CLI). Each addr should
    /// include a `/p2p/<peerid>` suffix so libp2p can authenticate
    /// the dialled peer.
    #[serde(default)]
    pub bootstrap: Vec<Multiaddr>,

    /// Gossipsub topics the relay will forward, as glob patterns.
    /// Default: `["qos.*"]` (every topic prefixed `qos.`). Use
    /// `["*"]` to forward everything (development only — exposes
    /// the relay to topic-flooding by random peers).
    #[serde(default = "default_topic_allowlist")]
    pub topic_allowlist: Vec<String>,

    /// Soft cap on messages per minute per (topic, peer) pair
    /// before a warning logs and the message is dropped. 0 disables
    /// the limiter. Defaults to 6 000 (≈100 msgs/sec/peer), which
    /// is generous enough that human-paced editing never trips it
    /// but tight enough to catch a runaway peer.
    #[serde(default = "default_rate_limit")]
    pub rate_limit_per_topic_per_minute: u32,

    /// Where to bind the Prometheus exposition endpoint. `None`
    /// (the default) disables metrics. When set, the relay serves
    /// `/metrics` on this socket; everything else returns `404`.
    #[serde(default)]
    pub metrics_listen: Option<SocketAddr>,

    /// Log level filter. Accepts `tracing_subscriber::EnvFilter`
    /// directives. Default: `"info"`. Common useful values:
    /// `"qos_relay=debug,libp2p=info"` to debug the relay loop
    /// without drowning in libp2p internals.
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

fn default_listen() -> Vec<Multiaddr> {
    vec![
        "/ip4/0.0.0.0/tcp/4001"
            .parse()
            .expect("hard-coded default multiaddr parses"),
        "/ip4/0.0.0.0/udp/4001/quic-v1"
            .parse()
            .expect("hard-coded default multiaddr parses"),
    ]
}

fn default_identity_path() -> PathBuf {
    PathBuf::from("/var/lib/qos-relay/identity.key")
}

fn default_topic_allowlist() -> Vec<String> {
    vec!["qos.*".to_string()]
}

fn default_rate_limit() -> u32 {
    6_000
}

fn default_log_level() -> String {
    "info".to_string()
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            identity_path: default_identity_path(),
            bootstrap: Vec::new(),
            topic_allowlist: default_topic_allowlist(),
            rate_limit_per_topic_per_minute: default_rate_limit(),
            metrics_listen: None,
            log_level: default_log_level(),
        }
    }
}

impl RelayConfig {
    /// Parse a config file at `path`. The file must be UTF-8 TOML.
    /// Missing fields fall back to the [`Default`] values.
    ///
    /// Errors surface with the file path included so multi-relay
    /// setups can identify which config tripped.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| {
            anyhow::anyhow!("failed to read config at {}: {}", path.display(), e)
        })?;
        Self::parse(&text).map_err(|e| {
            anyhow::anyhow!("failed to parse config at {}: {}", path.display(), e)
        })
    }

    /// Parse TOML config from a string. Useful for tests and for the
    /// `QOS_RELAY_CONFIG` env-var path where the caller has already
    /// done the file read.
    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// Serialise the config to a TOML string for round-trip tests and
    /// for printing the effective config at startup.
    pub fn to_toml(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }

    /// Validate the config after CLI overrides have been merged.
    /// Returns the first problem encountered, or `Ok(())`. The
    /// caller renders the message to the user; no auto-fix.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.listen.is_empty() {
            anyhow::bail!("listen must contain at least one multiaddr");
        }
        if self.topic_allowlist.is_empty() {
            anyhow::bail!("topic_allowlist must contain at least one glob pattern");
        }
        // Reject patterns that don't compile — surfaces typos at
        // startup instead of silently dropping every message.
        for pat in &self.topic_allowlist {
            globset::Glob::new(pat).map_err(|e| {
                anyhow::anyhow!("topic_allowlist pattern {pat:?} is invalid: {e}")
            })?;
        }
        Ok(())
    }

    /// Compile the topic-allowlist patterns into a [`GlobSet`] for
    /// fast lookup in the gossipsub message handler. Returns
    /// `None` when the allowlist is empty (which `validate` rejects,
    /// so callers should call `validate` first).
    pub fn compile_topic_matcher(&self) -> anyhow::Result<globset::GlobSet> {
        let mut builder = globset::GlobSetBuilder::new();
        for pat in &self.topic_allowlist {
            builder.add(globset::Glob::new(pat)?);
        }
        Ok(builder.build()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_self_consistent() {
        let cfg = RelayConfig::default();
        assert_eq!(cfg.listen.len(), 2);
        assert_eq!(cfg.topic_allowlist, vec!["qos.*"]);
        assert_eq!(cfg.rate_limit_per_topic_per_minute, 6_000);
        assert!(cfg.metrics_listen.is_none());
        assert_eq!(cfg.log_level, "info");
        cfg.validate().expect("default config must validate");
    }

    #[test]
    fn parse_empty_uses_defaults() {
        let cfg = RelayConfig::parse("").expect("empty parses");
        assert_eq!(cfg, RelayConfig::default());
    }

    #[test]
    fn parse_partial_inherits_other_defaults() {
        let cfg = RelayConfig::parse(r#"log_level = "debug""#).unwrap();
        assert_eq!(cfg.log_level, "debug");
        // Untouched fields hold their defaults.
        assert_eq!(cfg.topic_allowlist, vec!["qos.*"]);
    }

    #[test]
    fn validate_rejects_empty_listen() {
        let cfg = RelayConfig {
            listen: Vec::new(),
            ..RelayConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("listen"), "got: {err}");
    }

    #[test]
    fn validate_rejects_bad_glob() {
        let cfg = RelayConfig {
            // GlobBuilder rejects unmatched `[` — that's our invalid sentinel.
            topic_allowlist: vec!["qos.[".to_string()],
            ..RelayConfig::default()
        };
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("invalid"), "got: {err}");
    }

    #[test]
    fn compile_topic_matcher_works() {
        let cfg = RelayConfig {
            topic_allowlist: vec!["qos.*".to_string()],
            ..RelayConfig::default()
        };
        let m = cfg.compile_topic_matcher().expect("compiles");
        assert!(m.is_match("qos.channel.abc"));
        assert!(!m.is_match("other.topic"));
    }
}
