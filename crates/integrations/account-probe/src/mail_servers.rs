//! Where an address's mail lives, worked out from the address alone.
//!
//! A person who links a mail account says its address and nothing else. Its
//! servers are looked for in this order, and the first that answers is
//! taken:
//!
//! 1. the providers known by their name
//!    ([`accounts::email::AccountConfig::autofill_from_email`]), also where
//!    the servers that take the domain's mail are one of theirs;
//! 2. the settings document the address's own domain publishes for mail
//!    programs, then the ones its provider publishes: its provider is
//!    whoever takes the domain's mail or answers its autodiscover service
//!    name ([`Sources::documents`]);
//! 3. the settings the database of what providers publish keeps for the
//!    domain, and for its provider's ([`in_database`]);
//! 4. the services its domain names: the `SRV` names of RFC 6186 and
//!    RFC 8314 ([`Sources::service`]);
//! 5. the names mail servers are usually given (`imap.`, `mail.`, `smtp.`)
//!    under the domain and under its provider, each taken only where a mail
//!    server greets over a connection secured with a certificate for that
//!    very name ([`Sources::reads`], [`Sources::sends`]).
//!
//! Only when all of that finds nothing is a person asked to name a server.
//! Nothing here signs in and nothing here holds a credential: what is found
//! is where a sign-in is then proved ([`crate::AccountTester::test`]).
//!
//! What the domain name system answers is not signed, so a server it leads
//! to outside the address's own domain is whoever answered the lookup's to
//! say. Whoever asks for the password therefore shows the servers found
//! before it is typed, which is the confirmation RFC 6186 asks for.

use std::future::Future;
use std::net::IpAddr;
use std::sync::Arc;

use accounts::email::{AccountConfig, Security};
use hickory_resolver::proto::rr::RData;
use web_time::Duration;

/// The port IMAP is TLS from the first byte on.
const IMAPS: u16 = 993;
/// The port mail is handed in on over TLS from the first byte.
const SUBMISSIONS: u16 = 465;
/// The port mail is handed in on after an upgrade to TLS.
const SUBMISSION: u16 = 587;
/// How long one step of one look at one server may take.
const STEP: Duration = Duration::from_secs(4);
/// The longest name a server has.
const LONGEST_NAME: usize = 253;
/// The longest part of a name between two dots.
const LONGEST_LABEL: usize = 63;
/// The most domains looked under: the address's own, and its providers'.
const MOST_DOMAINS: usize = 4;
/// The most a settings document may hold, in bytes.
const MOST_DOCUMENT: usize = 64 * 1024;
/// Where the database of the settings providers publish is asked, by a
/// domain's name.
const DATABASE: &str = "autoconfig.thunderbird.net";

/// What is said of words that are no mail address.
pub const NOT_AN_ADDRESS: &str = "That is not a mail address";

/// One mail server: its name and its port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Server {
    /// The server's name, in lower case and without a dot at its end.
    pub name: String,
    pub port: u16,
}

impl Server {
    /// The server `name` at `port`, its name as it is compared.
    #[must_use]
    pub fn new(name: &str, port: u16) -> Self {
        Self {
            name: name.trim().trim_end_matches('.').to_ascii_lowercase(),
            port,
        }
    }
}

/// How an address's servers were found: the later of the two ways, where
/// its two servers were found in different ways.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Found {
    /// Its provider is known by name.
    Known,
    /// Its domain, or its provider, publishes them for mail programs.
    Published,
    /// Its domain names them as services.
    Service,
    /// A mail server greeted under a usual name, with a certificate for it.
    Names,
}

/// Where an address's mail is read and where it is handed in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailServers {
    /// The server its mail is read from (IMAP).
    pub incoming: Server,
    /// The server its mail is handed to (SMTP).
    pub outgoing: Server,
    pub found: Found,
    /// The name the account signs in with, where a published document says
    /// so; the address itself otherwise.
    pub username: Option<String>,
    /// The provider, by its name, when its accounts sign in on a page of its
    /// own and take no password from a mail program.
    pub own_sign_in: Option<&'static str>,
}

/// What a look for an address's servers asks of the network. Each answer may
/// be none; the look goes on to the next way.
pub trait Sources {
    /// Whether a server called `name` may be asked anything at all: a machine
    /// that reaches only the public internet says no to a name inside its
    /// own network.
    fn allowed(&self, _name: &str) -> bool {
        true
    }

    /// What each of the https addresses `urls` answers with, in their order:
    /// the text of a settings document, or none.
    fn documents(&self, urls: &[String]) -> Vec<Option<String>> {
        urls.iter().map(|_| None).collect()
    }

    /// The servers the service name `name` (such as `_imaps._tcp.example.com`)
    /// points to, the preferred first.
    fn service(&self, _name: &str) -> Vec<Server> {
        Vec::new()
    }

    /// The names of the servers that take mail for `domain`, the preferred
    /// first.
    fn exchangers(&self, _domain: &str) -> Vec<String> {
        Vec::new()
    }

    /// The first of `candidates`, in their order, at which an IMAP server
    /// greets over a connection secured with a certificate for its name.
    fn reads(&self, candidates: &[Server]) -> Option<Server>;

    /// The first of `candidates`, in their order, at which an SMTP server
    /// greets over a connection secured with a certificate for its name.
    fn sends(&self, candidates: &[Server]) -> Option<Server>;
}

/// Whether `name` is a server's name under a domain: letters, digits and
/// hyphens between dots, and at least one dot.
fn is_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= LONGEST_NAME
        && name.contains('.')
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= LONGEST_LABEL
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
        && name.parse::<IpAddr>().is_err()
}

/// The mail address `address` as its two halves, the domain in lower case;
/// none where the words are no address.
fn halves_of(address: &str) -> Option<(&str, String)> {
    let (local, domain) = address.rsplit_once('@')?;
    let domain = domain.to_ascii_lowercase();
    let written = !local.is_empty()
        && !local.chars().any(|character| {
            character.is_control() || character.is_whitespace() || character == '@'
        });
    (written && is_name(&domain)).then_some((local, domain))
}

/// Whether `name` is one of `domains` or lies under one.
fn under(name: &str, domains: &[&str]) -> bool {
    domains
        .iter()
        .any(|domain| name == *domain || name.ends_with(&format!(".{domain}")))
}

/// The provider whose accounts sign in on a page of its own, where `name` is
/// a server of one: what it is called, and a domain of its own.
fn own_page(name: &str) -> Option<(&'static str, &'static str)> {
    if under(name, &["gmail.com", "googlemail.com", "google.com"]) {
        Some(("Google", "gmail.com"))
    } else if under(
        name,
        &["outlook.com", "office365.com", "hotmail.com", "live.com"],
    ) {
        Some(("Microsoft", "outlook.com"))
    } else {
        None
    }
}

/// The servers of `address` where its provider is known by name. `provider`
/// is who takes its mail, where that is already known.
fn known(address: &str, provider: Option<&'static str>) -> Option<MailServers> {
    let mut config = AccountConfig {
        email: address.to_owned(),
        ..AccountConfig::default()
    };
    if !config.autofill_from_email() {
        return None;
    }
    let incoming = Server::new(&config.imap_host, config.imap_port);
    let outgoing = Server::new(&config.smtp_host, config.smtp_port);
    // A provider read through a program on the person's own machine is none
    // a server finds by name.
    (is_name(&incoming.name) && is_name(&outgoing.name)).then(|| MailServers {
        own_sign_in: provider.or_else(|| own_page(&incoming.name).map(|(called, _)| called)),
        incoming,
        outgoing,
        found: Found::Known,
        username: None,
    })
}

/// The domains a server called `name` lies under, the nearest first, as long
/// as two parts of the name are left: at most two.
fn above(name: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = name;
    while let Some((_, parent)) = rest.split_once('.') {
        if !parent.contains('.') || found.len() == 2 {
            break;
        }
        found.push(parent.to_owned());
        rest = parent;
    }
    found
}

/// The usual names under each of `domains`, each at each of `ports`: every
/// name at the first port before any name at the next. An empty first part
/// is the domain itself.
fn usual(domains: &[String], firsts: &[&str], ports: &[u16]) -> Vec<Server> {
    ports
        .iter()
        .flat_map(|port| {
            domains.iter().flat_map(move |domain| {
                firsts.iter().map(move |first| {
                    let name = if first.is_empty() {
                        domain.clone()
                    } else {
                        format!("{first}.{domain}")
                    };
                    Server::new(&name, *port)
                })
            })
        })
        .collect()
}

/// What the settings document `text` says of the account `address`: where
/// its mail is read, where it is handed in, and the name it signs in with.
/// A server that is TLS from the first byte comes before one that upgrades,
/// and one that is neither is none.
fn published_in(text: &str, address: &str) -> Option<(Server, Server, Option<String>)> {
    let document = roxmltree::Document::parse(text).ok()?;
    let (local, domain) = address.rsplit_once('@')?;
    let filled = |written: &str| {
        written
            .replace("%EMAILADDRESS%", address)
            .replace("%EMAILLOCALPART%", local)
            .replace("%EMAILDOMAIN%", domain)
    };
    let server = |element: &str, kind: &str| {
        let mut offered: Vec<(bool, Server, Option<String>)> = document
            .descendants()
            .filter(|node| node.has_tag_name(element) && node.attribute("type") == Some(kind))
            .filter_map(|node| {
                let said = |part: &str| {
                    node.children()
                        .find(|child| child.has_tag_name(part))
                        .and_then(|child| child.text())
                        .map(str::trim)
                };
                let name = filled(said("hostname")?);
                let port = said("port")?.parse::<u16>().ok()?;
                let upgrades = match said("socketType")? {
                    "SSL" => false,
                    "STARTTLS" => true,
                    _ => return None,
                };
                let username = said("username")
                    .map(&filled)
                    .filter(|name| !name.is_empty());
                Some((upgrades, Server::new(&name, port), username))
            })
            .collect();
        offered.sort_by_key(|(upgrades, _, _)| *upgrades);
        offered.into_iter().next()
    };
    let (_, incoming, username) = server("incomingServer", "imap")?;
    let (_, outgoing, _) = server("outgoingServer", "smtp")?;
    Some((incoming, outgoing, username))
}

/// The domains the servers of an address at `domain` are looked under: its
/// own first, then its providers', which are the domains above whoever
/// answers its autodiscover service name and whoever takes its mail.
fn looked_under(domain: &str, exchangers: &[String], sources: &dyn Sources) -> Vec<String> {
    let mut domains = vec![domain.to_owned()];
    let autodiscover = sources.service(&format!("_autodiscover._tcp.{domain}"));
    let leading = autodiscover
        .iter()
        .map(|server| &server.name)
        .chain(exchangers.iter());
    for name in leading {
        for parent in above(name) {
            if domains.len() < MOST_DOMAINS && !domains.contains(&parent) {
                domains.push(parent);
            }
        }
    }
    domains
}

/// What the first settings document found under `domains` says of
/// `address`. Only the address's own domain, the first, is told the address.
fn published(
    domains: &[String],
    address: &str,
    sources: &dyn Sources,
) -> Option<(Server, Server, Option<String>)> {
    let (local, _) = address.rsplit_once('@')?;
    let plain = local
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    let urls: Vec<String> = domains
        .iter()
        .enumerate()
        .flat_map(|(place, domain)| {
            let whose = if place == 0 && plain {
                format!("?emailaddress={address}")
            } else {
                String::new()
            };
            [
                (
                    format!("autoconfig.{domain}"),
                    format!("mail/config-v1.1.xml{whose}"),
                ),
                (
                    domain.clone(),
                    format!(".well-known/autoconfig/mail/config-v1.1.xml{whose}"),
                ),
            ]
        })
        .filter(|(name, _)| sources.allowed(name))
        .map(|(name, path)| format!("https://{name}/{path}"))
        .collect();
    if urls.is_empty() {
        return None;
    }
    sources
        .documents(&urls)
        .into_iter()
        .flatten()
        .find_map(|text| published_in(&text, address))
}

/// What the database of what providers publish says of the servers of
/// `address`: a settings document for each of `domains`, asked after the
/// domains' own documents and before the services a domain names, and only
/// where this machine may ask the database.
fn in_database(
    domains: &[String],
    address: &str,
    sources: &dyn Sources,
) -> Option<(Server, Server, Option<String>)> {
    if !sources.allowed(DATABASE) {
        return None;
    }
    let urls: Vec<String> = domains
        .iter()
        .map(|domain| format!("https://{DATABASE}/v1.1/{domain}"))
        .collect();
    sources
        .documents(&urls)
        .into_iter()
        .flatten()
        .find_map(|text| published_in(&text, address))
}

/// Where the mail of `address` lives, as `sources` answer.
///
/// # Errors
/// The words are no mail address, or no server was found for its domain:
/// said in words a person reads, which name what they would have to say.
pub fn find(address: &str, sources: &dyn Sources) -> Result<MailServers, String> {
    let address = address.trim();
    let (_, domain) = halves_of(address).ok_or_else(|| NOT_AN_ADDRESS.to_owned())?;
    if let Some(found) = known(address, None) {
        return Ok(found);
    }
    let exchangers: Vec<String> = sources
        .exchangers(&domain)
        .iter()
        .map(|name| Server::new(name, 0).name)
        .filter(|name| is_name(name))
        .collect();
    // A domain whose mail one of those providers takes signs in there too.
    if let Some((provider, own)) = exchangers.iter().find_map(|name| own_page(name))
        && let Some(found) = known(&format!("mail@{own}"), Some(provider))
    {
        return Ok(found);
    }
    let domains = looked_under(&domain, &exchangers, sources);
    // Only a name this machine may ask is asked, whoever named it.
    let asked = |candidates: Vec<Server>| -> Vec<Server> {
        candidates
            .into_iter()
            .filter(|server| is_name(&server.name) && sources.allowed(&server.name))
            .collect()
    };
    let reads = |found: Found, candidates: Vec<Server>| {
        let candidates = asked(candidates);
        (!candidates.is_empty())
            .then(|| sources.reads(&candidates))
            .flatten()
            .map(|server| (server, found))
    };
    let sends = |found: Found, candidates: Vec<Server>| {
        let candidates = asked(candidates);
        (!candidates.is_empty())
            .then(|| sources.sends(&candidates))
            .flatten()
            .map(|server| (server, found))
    };
    let said =
        published(&domains, address, sources).or_else(|| in_database(&domains, address, sources));
    let incoming = said
        .as_ref()
        .and_then(|(incoming, _, _)| reads(Found::Published, vec![incoming.clone()]))
        .or_else(|| {
            let named = sources.service(&format!("_imaps._tcp.{domain}"));
            reads(Found::Service, named)
        })
        .or_else(|| {
            let names = usual(&domains, &["imap", "mail", ""], &[IMAPS]);
            reads(Found::Names, names)
        });
    let outgoing = said
        .as_ref()
        .and_then(|(_, outgoing, _)| sends(Found::Published, vec![outgoing.clone()]))
        .or_else(|| {
            let mut named = sources.service(&format!("_submissions._tcp.{domain}"));
            named.extend(sources.service(&format!("_submission._tcp.{domain}")));
            sends(Found::Service, named)
        })
        .or_else(|| {
            let names = usual(&domains, &["smtp", "mail", ""], &[SUBMISSIONS, SUBMISSION]);
            sends(Found::Names, names)
        });
    match (incoming, outgoing) {
        (Some((incoming, read)), Some((outgoing, sent))) => Ok(MailServers {
            own_sign_in: own_page(&incoming.name).map(|(called, _)| called),
            username: said
                .filter(|_| read == Found::Published)
                .and_then(|(_, _, username)| username),
            incoming,
            outgoing,
            found: read.max(sent),
        }),
        (None, _) => Err(format!(
            "No mail server was found for {domain}: say the name of its incoming (IMAP) server"
        )),
        (Some(_), None) => Err(format!(
            "No server that takes mail in was found for {domain}: say the name of its outgoing \
             (SMTP) server"
        )),
    }
}

/// What a machine on the network answers. Mail servers are asked over a
/// secured connection whose certificate is checked against `roots`; a
/// settings document is read over https as the system trusts it.
pub struct Live<'a> {
    allowed: &'a dyn Fn(&str) -> bool,
    roots: rustls::RootCertStore,
    /// Where a mail server that is greeted and a settings document that is
    /// read may be: any address, unless [`Self::within`] says otherwise.
    reach: Option<crate::imap::Reach>,
}

/// What one settings document at `url` says, where it answers with one and
/// is reached `within` the look's reach: its address is judged at the
/// connection itself, whatever an earlier look-up of its name was answered.
fn fetched(url: &str, within: Option<&crate::imap::Reach>) -> Option<String> {
    let client = tilo_dav::DavClient::new(&tilo_dav::Options {
        timeout: STEP,
        connect_timeout: Some(STEP),
        // A document is its own address's to give: nothing is followed to another.
        redirects: tilo_dav::Redirects::Refuse,
        lookup: within.map(crate::imap::Reach::lookup),
        ..tilo_dav::Options::default()
    })
    .ok()?;
    let answered = client.get(url).ok()?;
    if !answered.status().is_success() {
        return None;
    }
    String::from_utf8(answered.read_bounded(MOST_DOCUMENT).ok()?).ok()
}

/// What `look` reads from the domain name system as this machine is set up
/// to ask it, within one step; nothing where it cannot be asked.
fn looked_up<T, F>(look: impl FnOnce(hickory_resolver::TokioResolver) -> F) -> Vec<T>
where
    F: Future<Output = Vec<T>>,
{
    let Ok(asking) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return Vec::new();
    };
    asking.block_on(async {
        let resolver = hickory_resolver::TokioResolver::builder_tokio()
            .ok()
            .and_then(|builder| builder.build().ok());
        let Some(resolver) = resolver else {
            return Vec::new();
        };
        tokio::time::timeout(STEP, look(resolver))
            .await
            .unwrap_or_default()
    })
}

impl<'a> Live<'a> {
    /// The network as this machine reaches it, asking only the servers
    /// `allowed` lets it ask, and trusting what the system trusts.
    #[must_use]
    pub fn new(allowed: &'a dyn Fn(&str) -> bool) -> Self {
        Self {
            allowed,
            roots: tilo_net::system_roots(),
            reach: None,
        }
    }

    /// The same, greeting mail servers and reading settings documents only
    /// within `reach`: what a server asks for, whose people name the
    /// addresses it looks at.
    #[must_use]
    pub fn within(mut self, reach: crate::imap::Reach) -> Self {
        self.reach = Some(reach);
        self
    }

    /// The same, trusting `roots` alone for mail servers: what a test's own
    /// servers present.
    #[must_use]
    pub fn trusting(mut self, roots: rustls::RootCertStore) -> Self {
        self.roots = roots;
        self
    }

    /// The first of `candidates`, in their order, that greets as a server
    /// that reads mail (`reading`) or takes it in. All are asked at once, so
    /// a name nobody answers under costs no more than one that answers.
    fn first(&self, candidates: &[Server], reading: bool) -> Option<Server> {
        let tls = Arc::new(tilo_net::client_config_with_roots(self.roots.clone()).ok()?);
        let asking = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        let reach = self.reach.clone().unwrap_or_else(crate::imap::Reach::any);
        asking.block_on(async {
            let looks: Vec<_> = candidates
                .iter()
                .cloned()
                .map(|server| {
                    let tls = Arc::clone(&tls);
                    let reach = reach.clone();
                    tokio::spawn(async move {
                        let greeted = if reading {
                            let security = Security::of_imap(true, server.port);
                            crate::imap::open(
                                &reach,
                                &server.name,
                                server.port,
                                security,
                                tls,
                                STEP,
                            )
                            .await
                            .map(drop)
                        } else {
                            crate::smtp::greets(&reach, &server.name, server.port, tls, STEP).await
                        };
                        greeted.is_ok().then_some(server)
                    })
                })
                .collect();
            for look in looks {
                if let Ok(Some(server)) = look.await {
                    return Some(server);
                }
            }
            None
        })
    }
}

impl Sources for Live<'_> {
    fn allowed(&self, name: &str) -> bool {
        (self.allowed)(name)
    }

    /// Every document is asked for at once, so the look waits one step for
    /// all of them.
    #[allow(
        clippy::needless_collect,
        reason = "every document is asked for before any answer is waited for"
    )]
    fn documents(&self, urls: &[String]) -> Vec<Option<String>> {
        std::thread::scope(|scope| {
            let within = self.reach.as_ref();
            let asked: Vec<_> = urls
                .iter()
                .map(|url| scope.spawn(move || fetched(url, within)))
                .collect();
            asked
                .into_iter()
                .map(|answer| answer.join().ok().flatten())
                .collect()
        })
    }

    fn service(&self, name: &str) -> Vec<Server> {
        let name = name.to_owned();
        looked_up(|resolver| async move {
            let Ok(found) = resolver.srv_lookup(name.as_str()).await else {
                return Vec::new();
            };
            let mut named: Vec<(u16, u16, Server)> = found
                .answers()
                .iter()
                .filter_map(|answer| match &answer.data {
                    RData::SRV(service) => Some((
                        service.priority,
                        service.weight,
                        Server::new(&service.target.to_utf8(), service.port),
                    )),
                    _ => None,
                })
                .collect();
            // The lowest priority first, and among equals the heavier.
            named.sort_by(|one, other| one.0.cmp(&other.0).then(other.1.cmp(&one.1)));
            named.into_iter().map(|(_, _, server)| server).collect()
        })
    }

    fn exchangers(&self, domain: &str) -> Vec<String> {
        let domain = domain.to_owned();
        looked_up(|resolver| async move {
            let Ok(found) = resolver.mx_lookup(domain.as_str()).await else {
                return Vec::new();
            };
            let mut named: Vec<(u16, String)> = found
                .answers()
                .iter()
                .filter_map(|answer| match &answer.data {
                    RData::MX(exchanger) => {
                        Some((exchanger.preference, exchanger.exchange.to_utf8()))
                    }
                    _ => None,
                })
                .collect();
            named.sort();
            named.into_iter().map(|(_, name)| name).collect()
        })
    }

    fn reads(&self, candidates: &[Server]) -> Option<Server> {
        self.first(candidates, true)
    }

    fn sends(&self, candidates: &[Server]) -> Option<Server> {
        self.first(candidates, false)
    }
}
