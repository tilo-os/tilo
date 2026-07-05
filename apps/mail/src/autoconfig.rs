//! Mozilla-style **autoconfig**: given only an email address, discover the
//! provider's IMAP/SMTP host, port, and security so the user rarely types
//! server details by hand.
//!
//! ## The parser (always compiled)
//! [`parse_autoconfig`] reads the `clientConfig` XML that Thunderbird's ISPDB
//! and provider `.well-known` endpoints serve, extracting the incoming (IMAP)
//! and outgoing (SMTP) server blocks. It is dependency-free (a tiny tag
//! scanner, no XML crate) and unit-tested against a canned document, so the
//! discovery logic is exercised in every build.
//!
//! ## The lookup (network, `live`)
//! [`discover`] queries the standard autoconfig locations in order and parses
//! the first that answers. It is gated behind `live`; offline (and `--shots`)
//! the setup form falls back to [`AccountConfig::autofill_from_email`]'s
//! heuristic guess.

use crate::model::{AccountConfig, AuthMethod};

/// Discovered connection settings for a domain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Autoconfig {
    pub display_name: Option<String>,
    pub imap_host: String,
    pub imap_port: u16,
    pub imap_tls: bool,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_tls: bool,
    /// True when the provider's `authentication` element asks for OAuth2 —
    /// the account then presets to [`AuthMethod::OAuth`].
    pub oauth: bool,
}

impl Autoconfig {
    /// Fold the discovered settings into a config, filling only the connection
    /// fields (leaving identity fields the user typed untouched).
    pub fn apply_to(&self, cfg: &mut AccountConfig) {
        cfg.imap_host = self.imap_host.clone();
        cfg.imap_port = self.imap_port;
        cfg.imap_tls = self.imap_tls;
        cfg.smtp_host = self.smtp_host.clone();
        cfg.smtp_port = self.smtp_port;
        cfg.smtp_tls = self.smtp_tls;
        if self.oauth {
            cfg.auth = AuthMethod::OAuth;
        }
        if cfg.display_name.trim().is_empty() {
            if let Some(name) = &self.display_name {
                cfg.display_name = name.clone();
            }
        }
    }
}

/// One parsed `<incomingServer>` / `<outgoingServer>` block.
struct Server {
    host: String,
    port: u16,
    tls: bool,
    oauth: bool,
}

/// Parse a Mozilla `clientConfig` autoconfig document. Returns `None` unless
/// **both** an IMAP incoming server and an SMTP outgoing server are present
/// with a hostname.
pub fn parse_autoconfig(xml: &str) -> Option<Autoconfig> {
    let incoming = extract_block(xml, "incomingServer", "imap").and_then(parse_server)?;
    let outgoing = extract_block(xml, "outgoingServer", "smtp").and_then(parse_server)?;
    if incoming.host.is_empty() || outgoing.host.is_empty() {
        return None;
    }
    let display_name = extract_block(xml, "emailProvider", "")
        .and_then(|b| inner_text(b, "displayName"))
        .filter(|s| !s.is_empty());
    Some(Autoconfig {
        display_name,
        imap_host: incoming.host,
        imap_port: incoming.port,
        imap_tls: incoming.tls,
        smtp_host: outgoing.host,
        smtp_port: outgoing.port,
        smtp_tls: outgoing.tls,
        oauth: incoming.oauth || outgoing.oauth,
    })
}

fn parse_server(block: &str) -> Option<Server> {
    let host = inner_text(block, "hostname").unwrap_or_default();
    let port = inner_text(block, "port").and_then(|p| p.parse().ok()).unwrap_or(0);
    let socket = inner_text(block, "socketType").unwrap_or_default().to_ascii_uppercase();
    // SSL / STARTTLS both encrypt; only an explicit "plain" is cleartext.
    let tls = !matches!(socket.as_str(), "PLAIN" | "NONE" | "");
    let auth = inner_text(block, "authentication").unwrap_or_default().to_ascii_lowercase();
    let oauth = auth.contains("oauth");
    Some(Server { host, port, tls, oauth })
}

/// Return the inner text of the first `<tag ...>` element in `xml`, or `None`.
/// Attribute-free open tags only (all autoconfig leaf elements are).
fn inner_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let start = xml.find(&open)? + open.len();
    let close = format!("</{tag}>");
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].trim().to_string())
}

/// Return the inner content of the first `<tag ...>` element whose opening tag
/// contains the attribute value `attr_value` (e.g. `type="imap"`). When
/// `attr_value` is empty, match the first `<tag ...>` regardless of attributes.
fn extract_block<'a>(xml: &'a str, tag: &str, attr_value: &str) -> Option<&'a str> {
    let open_prefix = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut from = 0usize;
    while let Some(rel) = xml[from..].find(&open_prefix) {
        let open_start = from + rel;
        // The byte right after the prefix must be a delimiter, so `<incoming>`
        // doesn't match the prefix of `<incomingServer`.
        let after = xml[open_start + open_prefix.len()..].chars().next();
        let is_boundary = matches!(after, Some(c) if c == ' ' || c == '>' || c == '\t' || c == '\n' || c == '/');
        let gt = match xml[open_start..].find('>') {
            Some(g) => open_start + g,
            None => return None,
        };
        let opening = &xml[open_start..gt];
        let attr_ok = attr_value.is_empty()
            || opening.contains(&format!("\"{attr_value}\""))
            || opening.contains(&format!("'{attr_value}'"));
        if is_boundary && attr_ok {
            let close_rel = xml[gt..].find(&close)? + gt;
            return Some(&xml[gt + 1..close_rel]);
        }
        from = gt + 1;
    }
    None
}

/// The autoconfig URLs to try, in priority order, for a domain + email.
/// Public so the (network) `discover` and its test can share the list.
pub fn discovery_urls(domain: &str, email: &str) -> Vec<String> {
    let enc_email = email.replace('@', "%40");
    vec![
        format!("https://autoconfig.{domain}/mail/config-v1.1.xml?emailaddress={enc_email}"),
        format!("https://{domain}/.well-known/autoconfig/mail/config-v1.1.xml?emailaddress={enc_email}"),
        format!("https://autoconfig.thunderbird.net/v1.1/{domain}"),
    ]
}

/// Discover settings for an email by querying the standard autoconfig
/// locations. Network path — compiled only under `live`.
#[cfg(feature = "live")]
pub fn discover(email: &str) -> Option<Autoconfig> {
    let (_, domain) = email.split_once('@')?;
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .user_agent("qOS Mail")
        .build()
        .ok()?;
    for url in discovery_urls(domain, email) {
        let Ok(resp) = client.get(&url).send() else { continue };
        if !resp.status().is_success() {
            continue;
        }
        let Ok(body) = resp.text() else { continue };
        if let Some(cfg) = parse_autoconfig(&body) {
            return Some(cfg);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<clientConfig version="1.1">
  <emailProvider id="example.com">
    <domain>example.com</domain>
    <displayName>Example Mail</displayName>
    <incomingServer type="imap">
      <hostname>imap.example.com</hostname>
      <port>993</port>
      <socketType>SSL</socketType>
      <authentication>password-cleartext</authentication>
      <username>%EMAILADDRESS%</username>
    </incomingServer>
    <outgoingServer type="smtp">
      <hostname>smtp.example.com</hostname>
      <port>587</port>
      <socketType>STARTTLS</socketType>
      <authentication>password-cleartext</authentication>
      <username>%EMAILADDRESS%</username>
    </outgoingServer>
  </emailProvider>
</clientConfig>"#;

    #[test]
    fn parses_hosts_ports_and_security() {
        let cfg = parse_autoconfig(SAMPLE).expect("valid autoconfig");
        assert_eq!(cfg.imap_host, "imap.example.com");
        assert_eq!(cfg.imap_port, 993);
        assert!(cfg.imap_tls);
        assert_eq!(cfg.smtp_host, "smtp.example.com");
        assert_eq!(cfg.smtp_port, 587);
        assert!(cfg.smtp_tls, "STARTTLS is still an encrypted socket");
        assert!(!cfg.oauth, "password-cleartext auth is not OAuth");
        assert_eq!(cfg.display_name.as_deref(), Some("Example Mail"));
    }

    #[test]
    fn apply_to_fills_connection_fields_only() {
        let cfg = parse_autoconfig(SAMPLE).unwrap();
        let mut acct = AccountConfig {
            email: "me@example.com".into(),
            username: "me@example.com".into(),
            ..Default::default()
        };
        cfg.apply_to(&mut acct);
        assert_eq!(acct.imap_host, "imap.example.com");
        assert_eq!(acct.smtp_port, 587);
        assert_eq!(acct.username, "me@example.com", "identity fields untouched");
        assert_eq!(acct.auth, AuthMethod::Password);
        // The empty display name is filled from the provider's displayName.
        assert_eq!(acct.display_name, "Example Mail");
    }

    #[test]
    fn detects_oauth_authentication() {
        let xml = SAMPLE.replace("password-cleartext", "OAuth2");
        let cfg = parse_autoconfig(&xml).expect("valid autoconfig");
        assert!(cfg.oauth, "OAuth2 authentication must set the oauth flag");
        let mut acct = AccountConfig::default();
        cfg.apply_to(&mut acct);
        assert_eq!(acct.auth, AuthMethod::OAuth);
    }

    #[test]
    fn rejects_document_missing_a_server() {
        // Only an incoming server — not enough to configure sending.
        let xml = r#"<clientConfig><emailProvider id="x">
            <incomingServer type="imap"><hostname>imap.x</hostname><port>993</port>
            <socketType>SSL</socketType></incomingServer>
            </emailProvider></clientConfig>"#;
        assert!(parse_autoconfig(xml).is_none());
    }

    #[test]
    fn ignores_pop3_incoming_and_requires_imap() {
        // A POP3-only provider block must not be mistaken for IMAP.
        let xml = r#"<clientConfig><emailProvider id="x">
            <incomingServer type="pop3"><hostname>pop.x</hostname><port>995</port>
            <socketType>SSL</socketType></incomingServer>
            <outgoingServer type="smtp"><hostname>smtp.x</hostname><port>465</port>
            <socketType>SSL</socketType></outgoingServer>
            </emailProvider></clientConfig>"#;
        assert!(parse_autoconfig(xml).is_none(), "no IMAP server → no config");
    }

    #[test]
    fn discovery_urls_cover_wellknown_and_ispdb() {
        let urls = discovery_urls("example.com", "me@example.com");
        assert!(urls.iter().any(|u| u.contains("autoconfig.example.com/mail/config-v1.1.xml")));
        assert!(urls.iter().any(|u| u.contains("example.com/.well-known/autoconfig")));
        assert!(urls.iter().any(|u| u.contains("autoconfig.thunderbird.net/v1.1/example.com")));
        assert!(urls.iter().all(|u| u.contains("me%40example.com") || u.ends_with("example.com")));
    }
}
