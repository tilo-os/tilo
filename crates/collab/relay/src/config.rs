//! TOML configuration schema for `qos-relay`.
//!
//! `RelayConfig::default()` is what ships when no TOML file is provided;
//! every field is also overrideable on the CLI (`qos-relay --listen ...
//! --bootstrap ... --log debug`). `serde(default)` per field means a
//! sparse TOML file only needs to set what it overrides.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use libp2p::Multiaddr;
use serde::{Deserialize, Serialize};

/// On-disk config for a relay instance. Every field has `serde(default)`,
/// so the `Default` impl is the source of truth for the out-of-the-box config.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct RelayConfig {
    /// Multiaddrs the relay listens on, each dialled via `Swarm::listen_on`. Defaults to dual-stack `0.0.0.0` on TCP and QUIC port 4001.
    #[serde(default = "default_listen")]
    pub listen: Vec<Multiaddr>,

    /// Persistent keypair path, generated on first run if missing. Default
    /// `/var/lib/qos-relay/identity.key` matches the systemd unit's `StateDirectory=qos-relay`.
    #[serde(default = "default_identity_path")]
    pub identity_path: PathBuf,

    /// Bootstrap multiaddrs to dial on startup, seeding the Kademlia routing
    /// table; an empty list means "I am the first relay" (slow rendezvous
    /// until peers connect via mdns). Each addr should carry a
    /// `/p2p/<peerid>` suffix so libp2p can authenticate the dial.
    #[serde(default)]
    pub bootstrap: Vec<Multiaddr>,

    /// Gossipsub topics to forward, as glob patterns. Default `["qos.*"]`;
    /// `["*"]` forwards everything (dev only — exposes the relay to
    /// topic-flooding by random peers).
    #[serde(default = "default_topic_allowlist")]
    pub topic_allowlist: Vec<String>,

    /// Soft cap on messages per minute per (topic, peer) before a warning
    /// logs and the message drops; 0 disables it. Default 6 000
    /// (≈100 msgs/sec/peer) — generous for human editing, tight enough to
    /// catch a runaway peer.
    #[serde(default = "default_rate_limit")]
    pub rate_limit_per_topic_per_minute: u32,

    /// Prometheus exposition bind address; `None` (default) disables
    /// metrics. When set, `/metrics` serves there and everything else 404s.
    #[serde(default)]
    pub metrics_listen: Option<SocketAddr>,

    /// Log level filter (`tracing_subscriber::EnvFilter` directives).
    /// Default `"info"`; e.g. `"relay=debug,libp2p=info"` to debug just the relay loop.
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
    /// Parses a config file at `path` (UTF-8 TOML); missing fields fall back
    /// to [`Default`]. Errors include the file path for multi-relay setups.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read config at {}: {}", path.display(), e))?;
        Self::parse(&text)
            .map_err(|e| anyhow::anyhow!("failed to parse config at {}: {}", path.display(), e))
    }

    /// Parses TOML config from a string; used by tests and the `QOS_RELAY_CONFIG` env-var path.
    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// Serialises the config to TOML, for round-trip tests and printing the effective config at startup.
    pub fn to_toml(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }

    /// Validates the config after CLI overrides are merged, returning the first problem found; the caller renders the message, no auto-fix.
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
            globset::Glob::new(pat)
                .map_err(|e| anyhow::anyhow!("topic_allowlist pattern {pat:?} is invalid: {e}"))?;
        }
        Ok(())
    }

    /// Compiles the topic-allowlist into a [`GlobSet`] for fast lookup in the
    /// gossipsub handler; call [`Self::validate`] first to reject bad patterns.
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
