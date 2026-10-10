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
    /// True when the provider's `authentication` element asks for `OAuth2` —
    /// the account then presets to [`AuthMethod::OAuth`].
    pub oauth: bool,
}

impl Autoconfig {
    /// Fold the discovered settings into a config, filling only the connection
    /// fields (leaving identity fields the user typed untouched).
    pub fn apply_to(&self, cfg: &mut AccountConfig) {
        cfg.imap_host.clone_from(&self.imap_host);
        cfg.imap_port = self.imap_port;
        cfg.imap_tls = self.imap_tls;
        cfg.smtp_host.clone_from(&self.smtp_host);
        cfg.smtp_port = self.smtp_port;
        cfg.smtp_tls = self.smtp_tls;
        if self.oauth {
            cfg.auth = AuthMethod::OAuth;
        }
        if cfg.display_name.trim().is_empty()
            && let Some(name) = &self.display_name
        {
            cfg.display_name.clone_from(name);
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
    let incoming = extract_block(xml, "incomingServer", "imap").map(parse_server)?;
    let outgoing = extract_block(xml, "outgoingServer", "smtp").map(parse_server)?;
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

fn parse_server(block: &str) -> Server {
    let host = inner_text(block, "hostname").unwrap_or_default();
    let port = inner_text(block, "port")
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    let socket = inner_text(block, "socketType")
        .unwrap_or_default()
        .to_ascii_uppercase();
    // SSL / STARTTLS both encrypt; only an explicit "plain" is cleartext.
    let tls = !matches!(socket.as_str(), "PLAIN" | "NONE" | "");
    let auth = inner_text(block, "authentication")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let oauth = auth.contains("oauth");
    Server {
        host,
        port,
        tls,
        oauth,
    }
}

/// Return the inner text of the first `<tag ...>` element in `xml`, or `None`.
/// Attribute-free open tags only (all autoconfig leaf elements are).
fn inner_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let start = xml.find(&open)? + open.len();
    let close = format!("</{tag}>");
    let end = xml.get(start..)?.find(&close)? + start;
    Some(xml.get(start..end)?.trim().to_string())
}

/// Return the inner content of the first `<tag ...>` element whose opening tag
/// contains the attribute value `attr_value` (e.g. `type="imap"`). When
/// `attr_value` is empty, match the first `<tag ...>` regardless of attributes.
fn extract_block<'a>(xml: &'a str, tag: &str, attr_value: &str) -> Option<&'a str> {
    let open_prefix = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut from = 0usize;
    while let Some(rel) = xml.get(from..)?.find(&open_prefix) {
        let open_start = from + rel;
        // The byte right after the prefix must be a delimiter, so `<incoming>`
        // doesn't match the prefix of `<incomingServer`.
        let after = xml.get(open_start + open_prefix.len()..)?.chars().next();
        let is_boundary =
            matches!(after, Some(c) if c == ' ' || c == '>' || c == '\t' || c == '\n' || c == '/');
        let gt = {
            let g = xml.get(open_start..)?.find('>')?;
            open_start + g
        };
        let opening = xml.get(open_start..gt)?;
        let attr_ok = attr_value.is_empty()
            || opening.contains(&format!("\"{attr_value}\""))
            || opening.contains(&format!("'{attr_value}'"));
        if is_boundary && attr_ok {
            let close_rel = xml.get(gt..)?.find(&close)? + gt;
            return xml.get(gt + 1..close_rel);
        }
        from = gt + 1;
    }
    None
}

/// A curated connection profile for a well-known provider, used when the
/// provider is identified by something other than its ISPDB XML — a Microsoft
/// consumer domain the ISPDB may not cover (`@outlook.de`, `@hotmail.co.uk`),
/// or an MX record that points at a known host. These presets require OAuth,
/// matching what Google and Microsoft now mandate for IMAP/SMTP.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KnownProvider {
    Google,
    Microsoft,
    Yahoo,
    Fastmail,
    ICloud,
}

impl KnownProvider {
    /// The provider's own name.
    pub fn name(self) -> &'static str {
        match self {
            KnownProvider::Google => "Google",
            KnownProvider::Microsoft => "Microsoft",
            KnownProvider::Yahoo => "Yahoo",
            KnownProvider::Fastmail => "Fastmail",
            KnownProvider::ICloud => "iCloud",
        }
    }

    /// The connection settings for this provider. OAuth is required for Google
    /// and Microsoft; the others accept an app-password.
    pub fn autoconfig(self) -> Autoconfig {
        match self {
            KnownProvider::Google => Autoconfig {
                display_name: None,
                imap_host: "imap.gmail.com".into(),
                imap_port: 993,
                imap_tls: true,
                smtp_host: "smtp.gmail.com".into(),
                smtp_port: 465,
                smtp_tls: true,
                oauth: true,
            },
            KnownProvider::Microsoft => Autoconfig {
                display_name: None,
                imap_host: "outlook.office365.com".into(),
                imap_port: 993,
                imap_tls: true,
                smtp_host: "smtp.office365.com".into(),
                smtp_port: 587,
                smtp_tls: true,
                oauth: true,
            },
            KnownProvider::Yahoo => Autoconfig {
                display_name: None,
                imap_host: "imap.mail.yahoo.com".into(),
                imap_port: 993,
                imap_tls: true,
                smtp_host: "smtp.mail.yahoo.com".into(),
                smtp_port: 465,
                smtp_tls: true,
                oauth: false,
            },
            KnownProvider::Fastmail => Autoconfig {
                display_name: None,
                imap_host: "imap.fastmail.com".into(),
                imap_port: 993,
                imap_tls: true,
                smtp_host: "smtp.fastmail.com".into(),
                smtp_port: 465,
                smtp_tls: true,
                oauth: false,
            },
            KnownProvider::ICloud => Autoconfig {
                display_name: None,
                imap_host: "imap.mail.me.com".into(),
                imap_port: 993,
                imap_tls: true,
                smtp_host: "smtp.mail.me.com".into(),
                smtp_port: 587,
                smtp_tls: true,
                oauth: false,
            },
        }
    }
}

/// Recognise a well-known provider from an email domain, covering the
/// Microsoft/Google **consumer domains across every TLD** (so `@outlook.de`,
/// `@hotmail.co.uk`, `@live.fr` all resolve to Microsoft — chosen by the
/// domain family, never by the `.com` TLD). Returns `None` for domains that
/// need real network discovery. Pure + unit-tested.
pub fn known_provider_for_domain(domain: &str) -> Option<KnownProvider> {
    let d = domain.to_ascii_lowercase();
    // The provider name is the leftmost label of the registrable domain, so
    // `outlook.de`, `hotmail.co.uk`, and `outlook.com` all reduce to their
    // family ("outlook"/"hotmail") regardless of TLD.
    let base = d.split('.').next().unwrap_or(&d);
    match base {
        "gmail" | "googlemail" => Some(KnownProvider::Google),
        "outlook" | "hotmail" | "live" | "msn" => Some(KnownProvider::Microsoft),
        "yahoo" | "ymail" | "rocketmail" => Some(KnownProvider::Yahoo),
        "fastmail" => Some(KnownProvider::Fastmail),
        "icloud" | "me" | "mac" => Some(KnownProvider::ICloud),
        _ => match d.as_str() {
            "office365.com" => Some(KnownProvider::Microsoft),
            _ => None,
        },
    }
}

/// A curated, **offline** autoconfig for common consumer providers, keyed by
/// email domain. This is the instant, network-free fill the setup form applies
/// the moment a recognised address is typed — so `me@gmail.com` pre-fills the
/// Gmail servers without any lookup, and it doubles as the offline fallback for
/// the [`discover`] network path. Covers the OAuth providers via
/// [`known_provider_for_domain`] plus the large European/free webmail hosts the
/// ISPDB may not resolve offline (GMX, web.de, mail.com, Zoho, AOL, Proton
/// bridge). Returns `None` for domains that need real discovery.
pub fn builtin_autoconfig(domain: &str) -> Option<Autoconfig> {
    // The OAuth / big-provider set first (chosen by domain family, any TLD).
    if let Some(p) = known_provider_for_domain(domain) {
        return Some(p.autoconfig());
    }
    let d = domain.to_ascii_lowercase();
    let base = d.split('.').next().unwrap_or(&d);
    // (imap_host, imap_port, imap_tls, smtp_host, smtp_port, smtp_tls)
    let (ih, ip, itls, sh, sp, stls) = match base {
        // GMX and web.de share Germany's 1&1 infrastructure (STARTTLS on 587).
        "gmx" => ("imap.gmx.net", 993, true, "mail.gmx.net", 587, true),
        "web" if d == "web.de" => ("imap.web.de", 993, true, "smtp.web.de", 587, true),
        "mail" if d == "mail.com" => ("imap.mail.com", 993, true, "smtp.mail.com", 587, true),
        "zoho" => ("imap.zoho.com", 993, true, "smtp.zoho.com", 465, true),
        "aol" => ("imap.aol.com", 993, true, "smtp.aol.com", 465, true),
        // Proton needs the local Bridge, which re-serves IMAP/SMTP on loopback.
        "proton" | "protonmail" | "pm" => ("127.0.0.1", 1143, true, "127.0.0.1", 1025, true),
        _ => return None,
    };
    Some(Autoconfig {
        display_name: None,
        imap_host: ih.into(),
        imap_port: ip,
        imap_tls: itls,
        smtp_host: sh.into(),
        smtp_port: sp,
        smtp_tls: stls,
        oauth: false,
    })
}

/// Map a resolved MX hostname onto a known provider. This is the authoritative
/// signal for **hosted** domains: a company on Google Workspace or Microsoft
/// 365 has its own vanity domain, but its MX points at the provider, so
/// `example.com` with MX `aspmx.l.google.com` resolves to Google. Pure +
/// unit-tested; the live `discover` feeds it the DNS answer.
pub fn provider_from_mx_host(mx_host: &str) -> Option<KnownProvider> {
    let h = mx_host.to_ascii_lowercase();
    let h = h.trim_end_matches('.');
    if h.ends_with("google.com")
        || h.ends_with("googlemail.com")
        || h.ends_with("aspmx.l.google.com")
    {
        Some(KnownProvider::Google)
    } else if h.ends_with("outlook.com")
        || h.ends_with("protection.outlook.com")
        || h.ends_with("office365.com")
    {
        Some(KnownProvider::Microsoft)
    } else if h.ends_with("yahoodns.net") || h.ends_with("yahoo.com") {
        Some(KnownProvider::Yahoo)
    } else if h.ends_with("messagingengine.com") || h.ends_with("fastmail.com") {
        Some(KnownProvider::Fastmail)
    } else if h.ends_with("icloud.com") || h.ends_with("me.com") || h.ends_with("mail.me.com") {
        Some(KnownProvider::ICloud)
    } else {
        None
    }
}

/// Pick the highest-priority (lowest-preference-number) provider from a set of
/// MX records. Returns the first host that maps to a known provider, scanning
/// in ascending preference. Pure + unit-tested.
pub fn provider_from_mx_records(mut records: Vec<(u16, String)>) -> Option<KnownProvider> {
    records.sort_by_key(|(pref, _)| *pref);
    records
        .iter()
        .find_map(|(_, host)| provider_from_mx_host(host))
}

/// Parse a DNS-over-HTTPS JSON answer (RFC 8484 / the `application/dns-json`
/// shape Google and Cloudflare serve) into `(preference, exchange)` MX records.
/// The `data` field of an MX answer is `"<pref> <host>"`. Pure + unit-tested;
/// the live `discover` fetches the JSON.
pub fn parse_doh_mx(json: &str) -> Vec<(u16, String)> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(answers) = v.get("Answer").and_then(|a| a.as_array()) else {
        return Vec::new();
    };
    answers
        .iter()
        // MX record type is 15.
        .filter(|a| a.get("type").and_then(serde_json::Value::as_u64) == Some(15))
        .filter_map(|a| {
            let data = a.get("data")?.as_str()?.trim();
            let (pref, host) = data.split_once(char::is_whitespace)?;
            Some((pref.trim().parse().ok()?, host.trim().to_string()))
        })
        .collect()
}

/// One DNS `SRV` answer (RFC 2782).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Srv {
    pub priority: u16,
    pub weight: u16,
    pub port: u16,
    pub host: String,
}

/// Parse a DNS-over-HTTPS JSON answer into its `SRV` records. A target of `.`
/// means the service is explicitly unavailable (RFC 2782) and is dropped.
pub fn parse_doh_srv(json: &str) -> Vec<Srv> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(answers) = v.get("Answer").and_then(|a| a.as_array()) else {
        return Vec::new();
    };
    answers
        .iter()
        // SRV record type is 33.
        .filter(|a| a.get("type").and_then(serde_json::Value::as_u64) == Some(33))
        .filter_map(|a| {
            let mut fields = a.get("data")?.as_str()?.split_whitespace();
            let priority = fields.next()?.parse().ok()?;
            let weight = fields.next()?.parse().ok()?;
            let port = fields.next()?.parse().ok()?;
            let host = fields.next()?.trim_end_matches('.').to_string();
            (!host.is_empty()).then_some(Srv {
                priority,
                weight,
                port,
                host,
            })
        })
        .collect()
}

/// The server to use among `records`: the lowest priority, then the heaviest.
fn best_srv(records: &[Srv]) -> Option<&Srv> {
    records
        .iter()
        .min_by_key(|r| (r.priority, std::cmp::Reverse(r.weight)))
}

/// A configuration from RFC 6186 service records: `_imaps` for incoming and
/// `_submissions` (implicit TLS) or else `_submission` for outgoing. Both
/// servers must be published.
pub fn autoconfig_from_srv(
    imaps: &[Srv],
    submissions: &[Srv],
    submission: &[Srv],
) -> Option<Autoconfig> {
    let imap = best_srv(imaps)?;
    let smtp = best_srv(submissions).or_else(|| best_srv(submission))?;
    Some(Autoconfig {
        display_name: None,
        imap_host: imap.host.clone(),
        imap_port: imap.port,
        imap_tls: true,
        smtp_host: smtp.host.clone(),
        smtp_port: smtp.port,
        smtp_tls: true,
        oauth: false,
    })
}

/// The autoconfig URLs to try, in priority order, for a domain + email.
/// Public so the (network) `discover` and its test can share the list.
pub fn discovery_urls(domain: &str, email: &str) -> Vec<String> {
    let enc_email = email.replace('@', "%40");
    vec![
        format!("https://autoconfig.{domain}/mail/config-v1.1.xml?emailaddress={enc_email}"),
        format!(
            "https://{domain}/.well-known/autoconfig/mail/config-v1.1.xml?emailaddress={enc_email}"
        ),
        format!("https://autoconfig.thunderbird.net/v1.1/{domain}"),
    ]
}

/// Discover settings for an email by querying the standard autoconfig
/// locations, then falling back to a curated known-provider table and finally
/// an MX-record lookup. Network path — compiled only under `live`. Returns
/// `None` when nothing matched, so the caller can prompt "enter details
/// manually".
#[cfg(feature = "live")]
pub fn discover(email: &str) -> Option<Autoconfig> {
    discover_cancellable(email, &std::sync::atomic::AtomicBool::new(false))
}

/// Discover on a worker, dropping active HTTP requests when setup closes.
#[cfg(feature = "live")]
pub fn discover_cancellable(
    email: &str,
    cancellation: &std::sync::atomic::AtomicBool,
) -> Option<Autoconfig> {
    let (_, domain) = email.split_once('@')?;
    discover_with_urls(email, cancellation, discovery_urls(domain, email), true)
}

#[cfg(feature = "live")]
fn discover_with_urls(
    email: &str,
    cancellation: &std::sync::atomic::AtomicBool,
    urls: Vec<String>,
    dns: bool,
) -> Option<Autoconfig> {
    let discovered = ui::rt::app().block_on(async {
        tokio::select! {
            biased;
            () = async {
                loop {
                    if cancellation.load(std::sync::atomic::Ordering::Relaxed) { return; }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            } => None,
            result = tokio::time::timeout(DISCOVERY_TIMEOUT, discover_candidates(email, urls, dns)) => result.ok().flatten(),
        }
    });
    discovered
        .inspect_err(|error| tracing::warn!(%error, "mail: autoconfig discovery could not run"))
        .ok()
        .flatten()
}

/// How long one lookup may take, and the whole discovery. Lookups run side by
/// side, so the slowest hit decides the wait, not their sum.
#[cfg(feature = "live")]
const LOOKUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
#[cfg(feature = "live")]
const DISCOVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Where a candidate configuration comes from, in priority order.
#[cfg(feature = "live")]
enum Source {
    /// A provider's autoconfig XML or the Thunderbird ISPDB.
    Url(String),
    /// RFC 6186 `_imaps` / `_submission(s)` SRV records.
    Srv,
    /// An MX host that names a known provider.
    Mx,
}

#[cfg(feature = "live")]
async fn discover_candidates(email: &str, urls: Vec<String>, dns: bool) -> Option<Autoconfig> {
    use futures_util::StreamExt as _;

    let (_, domain) = email.split_once('@')?;
    // The curated table answers without a socket.
    if let Some(cfg) = builtin_autoconfig(domain) {
        return Some(cfg);
    }
    let client = tilo_net::http::client()
        .timeout(LOOKUP_TIMEOUT)
        .user_agent("Tilo Mail")
        .build()
        .ok()?;
    // Every lookup starts at once; the answers are read in priority order, so
    // the first that has a configuration wins without waiting for the rest.
    // Dropping the stream (cancellation, a win) closes the other requests.
    let sources = urls.into_iter().map(Source::Url).chain(
        dns.then_some([Source::Srv, Source::Mx])
            .into_iter()
            .flatten(),
    );
    let mut lookups: futures_util::stream::FuturesOrdered<_> = sources
        .map(|source| resolve(&client, domain, source))
        .collect();
    while let Some(found) = lookups.next().await {
        if found.is_some() {
            return found;
        }
    }
    None
}

#[cfg(feature = "live")]
async fn resolve(client: &reqwest::Client, domain: &str, source: Source) -> Option<Autoconfig> {
    match source {
        Source::Url(url) => {
            let resp = client.get(&url).send().await.ok()?;
            if !resp.status().is_success() {
                return None;
            }
            parse_autoconfig(&resp.text().await.ok()?)
        }
        Source::Srv => {
            let (imaps, submissions, submission) = tokio::join!(
                srv_records(client, "_imaps", domain),
                srv_records(client, "_submissions", domain),
                srv_records(client, "_submission", domain),
            );
            autoconfig_from_srv(&imaps, &submissions, &submission)
        }
        Source::Mx => discover_via_mx(client, domain)
            .await
            .map(KnownProvider::autoconfig),
    }
}

/// Ask Google's and then Cloudflare's DNS-over-HTTPS resolver for `name`'s
/// records of `record_type`; the first usable answer body.
#[cfg(feature = "live")]
async fn doh_query(client: &reqwest::Client, name: &str, record_type: &str) -> Option<String> {
    let endpoints = [
        format!("https://dns.google/resolve?name={name}&type={record_type}"),
        format!("https://cloudflare-dns.com/dns-query?name={name}&type={record_type}"),
    ];
    for url in endpoints {
        let Ok(resp) = client
            .get(&url)
            .header("accept", "application/dns-json")
            .send()
            .await
        else {
            continue;
        };
        if !resp.status().is_success() {
            continue;
        }
        if let Ok(body) = resp.text().await {
            return Some(body);
        }
    }
    None
}

#[cfg(feature = "live")]
async fn srv_records(client: &reqwest::Client, service: &str, domain: &str) -> Vec<Srv> {
    match doh_query(client, &format!("{service}._tcp.{domain}"), "SRV").await {
        Some(body) => parse_doh_srv(&body),
        None => Vec::new(),
    }
}

/// Resolve `domain`'s MX records via DNS-over-HTTPS and map them to a known
/// provider. Network — `live` only.
#[cfg(feature = "live")]
async fn discover_via_mx(client: &reqwest::Client, domain: &str) -> Option<KnownProvider> {
    let body = doh_query(client, domain, "MX").await?;
    provider_from_mx_records(parse_doh_mx(&body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn mx_discovery_identifies_hosts_by_preference_and_cancels_cleanly() {
        // Phase `mx_records_identify_hosted_google_and_microsoft`.
        {
            // A vanity domain hosted on Google Workspace.
            let google = vec![
                (5, "alt1.aspmx.l.google.com".to_string()),
                (1, "aspmx.l.google.com".to_string()),
            ];
            assert_eq!(
                provider_from_mx_records(google),
                Some(KnownProvider::Google)
            );

            // Microsoft 365 tenant.
            let ms = vec![(0, "example-com.mail.protection.outlook.com".to_string())];
            assert_eq!(provider_from_mx_records(ms), Some(KnownProvider::Microsoft));

            // Self-hosted / unknown MX → no guess (caller prompts for manual).
            let unknown = vec![(10, "mail.self-hosted.example".to_string())];
            assert_eq!(provider_from_mx_records(unknown), None);
        }

        // Phase `mx_guess_respects_preference_order`.
        {
            // The lowest-preference (highest-priority) recognised host wins even
            // when a higher-preference record is unknown.
            let records = vec![
                (20, "backup.self-hosted.example".to_string()),
                (10, "aspmx.l.google.com".to_string()),
            ];
            assert_eq!(
                provider_from_mx_records(records),
                Some(KnownProvider::Google)
            );
        }

        // Phase `parse_doh_mx_reads_google_dns_json`.
        {
            // The exact shape dns.google returns for an MX query.
            let json = r#"{
              "Status": 0,
              "Answer": [
                {"name":"example.com.","type":15,"TTL":300,"data":"10 aspmx.l.google.com."},
                {"name":"example.com.","type":15,"TTL":300,"data":"20 alt1.aspmx.l.google.com."},
                {"name":"example.com.","type":46,"TTL":300,"data":"mx 8 2 300 ..."}
              ]
            }"#;
            let mx = parse_doh_mx(json);
            assert_eq!(mx.len(), 2, "only MX (type 15) records are kept");
            assert_eq!(mx[0], (10, "aspmx.l.google.com.".to_string()));
            assert_eq!(provider_from_mx_records(mx), Some(KnownProvider::Google));
        }

        // Phase `parse_doh_mx_tolerates_missing_answer`.
        {
            assert!(parse_doh_mx(r#"{"Status":3}"#).is_empty());
            assert!(parse_doh_mx("not json").is_empty());
        }

        // Phase `cancelling_discovery_closes_a_stalled_http_request`.
        #[cfg(feature = "live")]
        {
            use std::io::Read;
            use std::sync::{
                Arc,
                atomic::{AtomicBool, Ordering},
                mpsc,
            };
            use ui::time::Duration;
            /// Bounds a wait that ends as soon as its condition holds; only a hang reaches it.
            const HANG_BOUND: Duration = Duration::from_secs(30);
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!("http://{}/config.xml", listener.local_addr().unwrap());
            let cancellation = Arc::new(AtomicBool::new(false));
            let worker_cancellation = Arc::clone(&cancellation);
            let (sender, receiver) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                sender
                    .send(discover_with_urls(
                        "fixture@custom.invalid",
                        &worker_cancellation,
                        vec![url],
                        false,
                    ))
                    .unwrap();
            });
            let deadline = tilo_test_env::hang_deadline();
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(ui::time::now() < deadline, "discovery never connected");
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("local listener: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream.set_read_timeout(Some(HANG_BOUND)).unwrap();
            let mut request = [0; 4096];
            assert!(
                stream.read(&mut request).unwrap() > 0,
                "real HTTP request began"
            );
            cancellation.store(true, Ordering::Relaxed);
            assert!(
                receiver
                    .recv_timeout(tilo_test_env::hang_bound())
                    .unwrap()
                    .is_none()
            );
            stream
                .read_to_end(&mut Vec::new())
                .expect("cancellation closes the request socket");
            worker.join().unwrap();
        }
    }

    #[test]
    fn srv_records_make_an_account_when_both_servers_are_published() {
        let json = r#"{"Answer":[
            {"type":33,"data":"20 5 993 backup.example.com."},
            {"type":33,"data":"10 1 993 imap.example.com."},
            {"type":5,"data":"alias.example.com."}]}"#;
        let imaps = parse_doh_srv(json);
        assert_eq!(imaps.len(), 2, "only SRV (type 33) records are kept");
        let submission =
            parse_doh_srv(r#"{"Answer":[{"type":33,"data":"0 1 587 smtp.example.com."}]}"#);
        let cfg = autoconfig_from_srv(&imaps, &[], &submission).expect("both published");
        assert_eq!(
            (
                cfg.imap_host.as_str(),
                cfg.imap_port,
                cfg.smtp_host.as_str(),
                cfg.smtp_port
            ),
            ("imap.example.com", 993, "smtp.example.com", 587),
            "the lowest priority wins and the trailing dot is dropped"
        );
        assert!(
            autoconfig_from_srv(&imaps, &[], &[]).is_none(),
            "no outgoing server"
        );
        let off = parse_doh_srv(r#"{"Answer":[{"type":33,"data":"0 0 0 ."}]}"#);
        assert!(
            off.is_empty(),
            "a `.` target means the service is unavailable"
        );
        assert!(parse_doh_srv("not json").is_empty());
    }

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
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn autoconfig_documents_parse_to_imap_smtp_and_oauth_settings() {
        // Phase `parses_hosts_ports_and_security`.
        {
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

        // Phase `apply_to_fills_connection_fields_only`.
        {
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

        // Phase `detects_oauth_authentication`.
        {
            let xml = SAMPLE.replace("password-cleartext", "OAuth2");
            let cfg = parse_autoconfig(&xml).expect("valid autoconfig");
            assert!(cfg.oauth, "OAuth2 authentication must set the oauth flag");
            let mut acct = AccountConfig::default();
            cfg.apply_to(&mut acct);
            assert_eq!(acct.auth, AuthMethod::OAuth);
        }

        // Phase `rejects_document_missing_a_server`.
        {
            // Only an incoming server — not enough to configure sending.
            let xml = r#"<clientConfig><emailProvider id="x">
                <incomingServer type="imap"><hostname>imap.x</hostname><port>993</port>
                <socketType>SSL</socketType></incomingServer>
                </emailProvider></clientConfig>"#;
            assert!(parse_autoconfig(xml).is_none());
        }

        // Phase `ignores_pop3_incoming_and_requires_imap`.
        {
            // A POP3-only provider block must not be mistaken for IMAP.
            let xml = r#"<clientConfig><emailProvider id="x">
                <incomingServer type="pop3"><hostname>pop.x</hostname><port>995</port>
                <socketType>SSL</socketType></incomingServer>
                <outgoingServer type="smtp"><hostname>smtp.x</hostname><port>465</port>
                <socketType>SSL</socketType></outgoingServer>
                </emailProvider></clientConfig>"#;
            assert!(
                parse_autoconfig(xml).is_none(),
                "no IMAP server → no config"
            );
        }

        // Phase `microsoft_ispdb_parses_to_oauth_office365`.
        {
            let cfg = parse_autoconfig(MICROSOFT_ISPDB).expect("valid microsoft autoconfig");
            assert_eq!(cfg.imap_host, "outlook.office365.com");
            assert_eq!(cfg.smtp_host, "smtp.office365.com");
            assert_eq!(cfg.smtp_port, 587);
            assert!(cfg.imap_tls && cfg.smtp_tls);
            assert!(cfg.oauth, "Microsoft consumer mail requires OAuth");
            // Applying it to an @outlook.de account presets OAuth.
            let mut acct = AccountConfig {
                email: "person@outlook.de".into(),
                ..Default::default()
            };
            cfg.apply_to(&mut acct);
            assert_eq!(acct.auth, AuthMethod::OAuth);
            assert_eq!(acct.imap_host, "outlook.office365.com");
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn provider_tables_cover_discovery_urls_and_known_domains() {
        // Phase `discovery_urls_cover_wellknown_and_ispdb`.
        {
            let urls = discovery_urls("example.com", "me@example.com");
            assert!(
                urls.iter()
                    .any(|u| u.contains("autoconfig.example.com/mail/config-v1.1.xml"))
            );
            assert!(
                urls.iter()
                    .any(|u| u.contains("example.com/.well-known/autoconfig"))
            );
            assert!(
                urls.iter()
                    .any(|u| u.contains("autoconfig.thunderbird.net/v1.1/example.com"))
            );
            assert!(
                urls.iter()
                    .all(|u| u.contains("me%40example.com") || u.ends_with("example.com"))
            );
        }

        // Phase `builtin_table_fills_common_providers_offline`.
        {
            // The offline, network-free fill the form applies as the address is
            // typed. Gmail → OAuth Gmail servers.
            let g = builtin_autoconfig("gmail.com").expect("gmail is built in");
            assert_eq!(g.imap_host, "imap.gmail.com");
            assert_eq!(g.imap_port, 993);
            assert_eq!(g.smtp_host, "smtp.gmail.com");
            assert!(g.oauth, "Gmail presets to OAuth");
            // Outlook across TLDs → office365.
            let o = builtin_autoconfig("outlook.de").expect("outlook is built in");
            assert_eq!(o.imap_host, "outlook.office365.com");
            assert_eq!(o.smtp_host, "smtp.office365.com");
            // The large free webmail hosts the ISPDB may miss offline.
            assert_eq!(
                builtin_autoconfig("gmx.net").unwrap().imap_host,
                "imap.gmx.net"
            );
            assert_eq!(
                builtin_autoconfig("web.de").unwrap().smtp_host,
                "smtp.web.de"
            );
            assert_eq!(
                builtin_autoconfig("fastmail.com").unwrap().imap_host,
                "imap.fastmail.com"
            );
            assert_eq!(
                builtin_autoconfig("yahoo.co.uk").unwrap().imap_host,
                "imap.mail.yahoo.com"
            );
            assert_eq!(
                builtin_autoconfig("icloud.com").unwrap().imap_host,
                "imap.mail.me.com"
            );
            assert_eq!(
                builtin_autoconfig("zoho.com").unwrap().smtp_host,
                "smtp.zoho.com"
            );
            // Encryption is on for every built-in provider.
            for d in [
                "gmail.com",
                "outlook.com",
                "gmx.net",
                "web.de",
                "zoho.com",
                "aol.com",
            ] {
                let ac = builtin_autoconfig(d).unwrap();
                assert!(ac.imap_tls && ac.smtp_tls, "{d} must use TLS");
            }
            // An unknown vanity domain falls through to real discovery.
            assert!(builtin_autoconfig("some-company.example").is_none());
        }

        // Phase `known_provider_covers_microsoft_consumer_domains_across_tlds`.
        {
            // The whole point: pick by domain family, not the .com TLD.
            assert_eq!(
                known_provider_for_domain("outlook.de"),
                Some(KnownProvider::Microsoft)
            );
            assert_eq!(
                known_provider_for_domain("hotmail.co.uk"),
                Some(KnownProvider::Microsoft)
            );
            assert_eq!(
                known_provider_for_domain("live.fr"),
                Some(KnownProvider::Microsoft)
            );
            assert_eq!(
                known_provider_for_domain("Gmail.com"),
                Some(KnownProvider::Google)
            );
            assert_eq!(
                known_provider_for_domain("yahoo.co.jp"),
                Some(KnownProvider::Yahoo)
            );
            // An unknown vanity domain must fall through to network discovery.
            assert_eq!(known_provider_for_domain("some-company.de"), None);
        }

        // Phase `microsoft_known_provider_config_is_oauth`.
        {
            let cfg = KnownProvider::Microsoft.autoconfig();
            assert_eq!(cfg.imap_host, "outlook.office365.com");
            assert_eq!(cfg.smtp_host, "smtp.office365.com");
            assert!(cfg.oauth, "Microsoft preset must require OAuth");
        }
    }

    // The real ISPDB document for outlook.com — an `@outlook.de` address must
    // parse it into a Microsoft OAuth profile (chosen by provider, not TLD).
    const MICROSOFT_ISPDB: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<clientConfig version="1.1">
  <emailProvider id="outlook.com">
    <domain>outlook.com</domain>
    <domain>outlook.de</domain>
    <domain>hotmail.com</domain>
    <displayName>Microsoft</displayName>
    <incomingServer type="imap">
      <hostname>outlook.office365.com</hostname>
      <port>993</port>
      <socketType>SSL</socketType>
      <authentication>OAuth2</authentication>
      <username>%EMAILADDRESS%</username>
    </incomingServer>
    <outgoingServer type="smtp">
      <hostname>smtp.office365.com</hostname>
      <port>587</port>
      <socketType>STARTTLS</socketType>
      <authentication>OAuth2</authentication>
      <username>%EMAILADDRESS%</username>
    </outgoingServer>
  </emailProvider>
</clientConfig>"#;
}
