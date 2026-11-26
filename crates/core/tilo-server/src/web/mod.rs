//! The host's browser listener (feature `web`): HTTPS (TCP, HTTP/2 and
//! HTTP/1.1), served by the server itself.
//!
//! What a page needs before it holds a session rides it: the host's
//! certificate and address (`/host`), joining a workspace, the public
//! directory, profiles, the fetch proxy and the static
//! entry. Everything after that — certification, presence, membership,
//! records, previews, calls — goes over iroh, as every device's does. Routes are listed in [`tilo_server_protocol::web`].
//!
//! No owner loop runs here: every handler is async and awaits the server's
//! own locks; workspace I/O already runs on the blocking pool.

mod content;
pub(crate) mod control;
mod fetch;
pub mod install;
mod link;
pub(crate) mod live;
mod page;
mod page_errors;
mod profiles;
#[cfg(test)]
mod tests;
mod tls;

pub use fetch::FetchProxySetup;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
pub use tls::TlsSource;

use bytes::Bytes;
use http_body_util::BodyExt as _;
use hyper::{Method, Request, Response, StatusCode, header};
use tilo_lock::MutexRecover as _;
pub(crate) use tilo_server_protocol::web as protocol;
use tilo_server_protocol::web::{
    API_PREFIX, MAX_REQUEST_BYTES, ServerAddress, ServerInfo, WebLink,
};

use crate::daemon::{Server, ServerError, WebContext};
use collab_types::WorkspaceId;

/// Configuration of the browser listener.
#[derive(Clone, Debug)]
pub struct WebConfig {
    /// TCP address for HTTPS. Port 0 picks one free.
    pub listen: SocketAddr,
    /// Certificate source.
    pub tls: TlsSource,
    /// Page origins allowed to call the host cross-origin (the Cloud console
    /// origin, a dev server). Empty allows same-origin pages only.
    pub allowed_origins: Vec<String>,
    /// A loopback address that also serves the HTTPS routes as plain HTTP,
    /// for local review in a browser that should not need a certificate
    /// exception.
    pub plain_listen: Option<SocketAddr>,
    /// The `https://` origin a TLS-terminating proxy publishes the plain
    /// listener at. When set, the plain listener may bind a non-loopback
    /// address (the proxy is the way in), and this origin is what pages
    /// and links are told.
    pub public_origin: Option<String>,
    /// The fetch proxy this host serves its members' pages at
    /// [`protocol::FETCH_PATH`], if any.
    pub fetch_proxy: Option<FetchProxySetup>,
    /// Where the host takes WebTransport sessions, when not on the UDP port of
    /// [`Self::listen`] at the origin pages are told: a deployment whose
    /// HTTPS arrives through a proxy that carries no UDP names a way around
    /// it. `None` listens beside the HTTPS listener.
    pub link: Option<LinkListen>,
    /// The site this host belongs to (`https://tilo.sh/`), when it knows one:
    /// a browser that opens the host's own address is sent to the site's
    /// page.
    pub site: Option<String>,
}

/// A WebTransport listener of its own ([`WebConfig::link`]).
#[derive(Clone, Debug)]
pub struct LinkListen {
    /// The UDP address it binds.
    pub listen: SocketAddr,
    /// The `https://` origin devices open its sessions at.
    pub origin: String,
}

impl Web {
    /// Tell a person no Tilo of theirs is open for that a question waits, in
    /// the browsers they let notify them. The push services are told this
    /// host by the site it belongs to, else its own origin. Only an assistant tells, so a host
    /// without one serves none.
    #[cfg(feature = "agents")]
    fn serve_pushes(
        &self,
        state: &std::path::Path,
        site: Option<&str>,
        public_origin: Option<&str>,
        addr: SocketAddr,
    ) {
        let contact = site
            .or(public_origin)
            .map_or_else(|| format!("https://{addr}"), str::to_owned);
        match crate::web_push::WebPush::open(state, &contact) {
            Ok(push) => {
                // What a person's assistant told them reaches the same
                // browsers, and the closed apps of theirs that take web
                // push, while no device of theirs is looking.
                if let (Some(workspaces), Ok(runtime)) =
                    (&self.workspaces, tokio::runtime::Handle::try_current())
                {
                    let push = push.telling(workspaces.clone());
                    let teller = tilo_app_host::agents::telling::Teller::new(
                        workspaces.clone(),
                        Arc::new(push),
                    );
                    runtime.spawn(teller.run());
                }
            }
            Err(error) => {
                tracing::warn!(%error, "this host tells nobody outside Tilo that a question waits");
            }
        }
    }
}

/// A running browser listener. Dropping it stops serving.
pub struct WebHandle {
    /// HTTPS address.
    pub addr: SocketAddr,
    /// `https://host:port` of the listener.
    pub origin: String,
    /// SHA-256 of a development certificate.
    pub certificate_sha256: Option<String>,
    /// Where pages reach the host over WebTransport, as `/host` publishes it.
    pub link: WebLink,
    /// `http://localhost:port` of the plain loopback listener, if any.
    pub plain_origin: Option<String>,
    /// Who this host is and where its endpoint is dialled, as `/host` answers it.
    server_info: ServerInfo,
    tasks: Arc<WebTasks>,
    /// The live session: the one account of who is here.
    presence: Arc<live::Authority>,
}

/// What a listener runs, shared with whoever stops it before the handle is
/// dropped: the device's `web.serve` setting stops it at once.
pub(crate) struct WebTasks {
    tasks: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
    locator_origin: Arc<std::sync::RwLock<Option<Arc<String>>>>,
    published_origin: Option<Arc<String>>,
    stopped: std::sync::atomic::AtomicBool,
}

impl WebTasks {
    /// Stop serving: every task is aborted and the browser locators name no origin of
    /// this listener any more. Stopping twice does nothing the second time.
    fn stop(&self) -> Vec<tokio::task::JoinHandle<()>> {
        if self.stopped.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return Vec::new();
        }
        if let Ok(mut published) = self.locator_origin.write()
            && self.published_origin.as_ref().is_some_and(|ours| {
                published
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(ours, current))
            })
        {
            *published = None;
        }
        let tasks = std::mem::take(
            &mut *self
                .tasks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for task in &tasks {
            task.abort();
        }
        tasks
    }
}

impl WebHandle {
    /// Who this host is and where its endpoint is dialled, as `/host` answers it.
    #[must_use]
    pub fn host_info(&self) -> &ServerInfo {
        &self.server_info
    }

    /// Open browser sessions per person (user root) now, for a public
    /// host's presence count and idle-guest pruning. One person on two
    /// devices is one entry; the workspace's visitor person is one entry whose
    /// count is its open pages.
    #[must_use]
    pub fn online_users(&self) -> std::collections::BTreeMap<[u8; 32], usize> {
        self.presence
            .devices()
            .into_iter()
            .map(|(user, devices)| (user, devices.len()))
            .collect()
    }

    /// What this listener runs, which stops it early.
    pub(crate) fn tasks(&self) -> Arc<WebTasks> {
        Arc::clone(&self.tasks)
    }
}

impl Drop for WebHandle {
    fn drop(&mut self) {
        self.tasks.stop();
    }
}

struct Web {
    context: WebContext,
    server_info: ServerInfo,
    /// The endpoint `server_info`'s address is of, whose current address `/host`
    /// answers ([`Web::current_host_info`]). `None` where the listener serves
    /// no endpoint of its own.
    endpoint: Option<iroh::Endpoint>,
    allowed_origins: Vec<String>,
    /// The workspaces this server serves, for their directory and entries.
    workspaces: Option<crate::workspaces::Workspaces>,
    /// Members' profiles, for `/profile`.
    profiles: Arc<profiles::Profiles>,
    /// The fetch proxy, when [`WebConfig::fetch_proxy`] asked for one, and the
    /// tokens this host's pages ask for it.
    fetch: Option<tilo_net::proxy::host::FetchProxy>,
    grants: Arc<tilo_workspace_service::fetch_grants::Grants>,
    /// The links to the workspaces' files this host serves.
    content_links: Arc<crate::content_links::ContentLinks>,
    /// The live session: the one account of who is here.
    presence: Arc<live::Authority>,
    /// The page errors the entry logs.
    page_errors: page_errors::PageErrors,
    /// The site this host belongs to, when it knows one: where a browser that
    /// opened a link at the host's own address is sent.
    site: Option<Site>,
    /// Where this host keeps each workspace's wait list's outbox.
    state_root: std::path::PathBuf,
    /// The wait lists this host serves, built from each workspace's entry when
    /// a page asks for one.
    served_lists: ServedLists,
}

/// What a workspace's entry says of its wait list: how many addresses it
/// keeps, where to ask off it, and where it mails from.
type ServedEntrySettings = (usize, Option<String>, Option<tilo_action_policy::EntryMail>);

/// A workspace's wait list as it was built, and the list itself.
type ServedList = (ServedEntrySettings, Option<Arc<crate::entry::WaitListPage>>);

/// What `entry` says of its wait list.
fn served_settings(entry: &tilo_action_policy::EntryPolicy) -> ServedEntrySettings {
    (
        entry.wait_list_max,
        entry.contact.clone(),
        entry.mail.clone(),
    )
}

/// The wait lists a host serves, built from each workspace's entry the first
/// time a page asks for one, and again when the entry changes
/// ([`crate::entry::WaitListPage`]).
#[derive(Default)]
struct ServedLists {
    /// A mailer that sends in place of the deployment's Resend sender, as a
    /// check supplies one.
    mailer: Option<Arc<dyn crate::entry::mail::Mailer>>,
    /// The entry each page was built from, and the page (none when the entry
    /// keeps no wait list).
    served: std::sync::Mutex<std::collections::HashMap<String, ServedList>>,
}

impl ServedLists {
    #[cfg(test)]
    fn with_mailer(mailer: Arc<dyn crate::entry::mail::Mailer>) -> Self {
        Self {
            mailer: Some(mailer),
            ..Self::default()
        }
    }
}

impl Web {
    /// The wait list `workspace`'s entry keeps now, as its entry settles it:
    /// built the first time a page asks for one and again when the entry
    /// changes. `None` when the entry keeps none.
    ///
    /// # Errors
    ///
    /// The entry names mail this host cannot send: its key file, its
    /// templates or the outbox's directory.
    async fn wait_list(
        &self,
        workspaces: &crate::workspaces::Workspaces,
        workspace: &str,
    ) -> anyhow::Result<Option<Arc<crate::entry::WaitListPage>>> {
        let Some(entry) = workspaces.entry_policy(workspace).await else {
            return Ok(None);
        };
        if !entry.keeps_requests() {
            return Ok(None);
        }
        let settings = served_settings(&entry);
        if let Some((built, page)) = self.served_lists.served.lock_recover().get(workspace)
            && *built == settings
        {
            return Ok(page.clone());
        }
        let name = workspaces
            .summary(workspace)
            .await
            .map(|summary| summary.name)
            .unwrap_or_default();
        // The entry's page, which the mails' links open: the site this host
        // belongs to, ending in `/`, or the site every link opens without one.
        let entry_page = match &self.site {
            Some(site) => format!("{}/", site.origin),
            None => crate::entry::ENTRY_WITHOUT_SITE.to_owned(),
        };
        let wait_list = crate::entry::WaitListPage::open(
            workspaces,
            workspace,
            &name,
            &self.state_root,
            &entry_page,
            &entry,
            self.served_lists.mailer.clone(),
        )?
        .map(Arc::new);
        // The invitations members ask the entry to mail go out from the page that
        // mails the addresses it keeps.
        if let Some(wait_list) = &wait_list
            && let Some(mut invitations) = wait_list.invitations(workspaces)
        {
            tokio::spawn(async move { while invitations.mail_next().await {} });
        }
        self.served_lists
            .served
            .lock_recover()
            .insert(workspace.to_owned(), (settings, wait_list.clone()));
        Ok(wait_list)
    }

    /// What `/host` answers: who this host is, and where its endpoint is dialled now. A
    /// device dials the host at this address, so it follows the endpoint: a relay the endpoint
    /// reached only after the listener started (its first network report found none, as when
    /// the host's own relay is not routable yet) is named as soon as it is.
    fn current_host_info(&self) -> ServerInfo {
        host_info_now(&self.server_info, self.endpoint.as_ref())
    }
}

/// `info`, naming where `endpoint` is dialled now ([`Web::current_host_info`]).
fn host_info_now(info: &ServerInfo, endpoint: Option<&iroh::Endpoint>) -> ServerInfo {
    let mut info = info.clone();
    if let (Some(endpoint), Some(address)) = (endpoint, info.address.as_mut()) {
        follow_endpoint(address, endpoint.addr());
    }
    info
}

/// Move `address` to the endpoint's address `now`. An endpoint that has lost its relays keeps
/// the relay `address` named, as a host keeps what it named elsewhere
/// (`tilo_endpoint_reachability::own_address::follow`): it is reached there again once the
/// relay is back, and an address with no relay reaches it from nowhere.
fn follow_endpoint(address: &mut ServerAddress, now: iroh_base::EndpointAddr) {
    let relay = now.relay_urls().next().map(ToString::to_string);
    if relay.is_none() && address.relay_url.is_some() {
        return;
    }
    address.relay_url = relay;
    address.endpoint_addr = now;
}

/// The site a host belongs to, as a link opened at the host's own address is
/// answered: the page that takes it is the site's.
#[derive(Clone, Debug)]
struct Site {
    /// The site's origin (`https://tilo.sh`).
    origin: String,
    /// The `host:port` a request names when it already reached the site, and so
    /// arrived through it: sending it on would be sending it to itself.
    authority: String,
}

impl Site {
    /// `entry` (`https://tilo.sh/`) as the site it names, if it names one.
    fn new(entry: &str) -> Option<Self> {
        let entry = entry.trim().trim_end_matches('/');
        let uri: hyper::Uri = entry.parse().ok()?;
        let authority = without_default_port(uri.authority()?.as_str()).to_owned();
        Some(Self {
            origin: entry.to_owned(),
            authority,
        })
    }

    /// Whether the request reached the host through the site itself.
    fn served_it(&self, request: &Request<hyper::body::Incoming>) -> bool {
        request
            .headers()
            .get(header::HOST)
            .and_then(|host| host.to_str().ok())
            .is_some_and(|host| without_default_port(host).eq_ignore_ascii_case(&self.authority))
    }
}

/// `authority` without the port its scheme implies (`tilo.sh:443` is
/// `tilo.sh`), which a browser leaves out of a `Host` as often as it writes it.
fn without_default_port(authority: &str) -> &str {
    authority
        .strip_suffix(":443")
        .or_else(|| authority.strip_suffix(":80"))
        .unwrap_or(authority)
}

/// Checks the published origins against the listeners they front. `also` are
/// the other origins whoever keeps this server says it is reached under: each
/// an origin as the public one is, beside which it stands.
fn checked_origins(config: &WebConfig, also: &[String]) -> Result<(), String> {
    if config.public_origin.is_none()
        && config
            .plain_listen
            .is_some_and(|plain| !plain.ip().is_loopback())
    {
        return Err(
            "the plain HTTP listener must be a loopback address unless a proxy publishes it".into(),
        );
    }
    if config.public_origin.as_ref().is_some_and(|origin| {
        !tilo_net::Url::parse(origin).is_ok_and(|url| tilo_net::secure_origin(&url))
            || config.plain_listen.is_none()
    }) {
        return Err(
            "a public origin is an https:// origin (or an http:// one on this machine) published for the plain listener".into(),
        );
    }
    if !also.is_empty() && config.public_origin.is_none() {
        return Err(
            "another origin this server is reached under stands beside its public origin, which is not set".into(),
        );
    }
    if !also
        .iter()
        .all(|origin| tilo_net::Url::parse(origin).is_ok_and(|url| tilo_net::secure_origin(&url)))
    {
        return Err(
            "another origin this server is reached under is an https:// origin (or an http:// one on this machine)".into(),
        );
    }
    Ok(())
}

/// An unspecified listener is not a usable browser destination. Only its
/// configured certificate host can provide one; no caller URL is accepted.
fn browser_locator_origin(
    tls: &TlsSource,
    addr: SocketAddr,
) -> Result<Option<String>, ServerError> {
    let host = match tls {
        TlsSource::Files { host, .. } => host
            .clone()
            .or_else(|| (!addr.ip().is_unspecified()).then(|| addr.ip().to_string())),
        TlsSource::Development { hosts, .. } => {
            let bound = addr.ip().to_string();
            if !addr.ip().is_unspecified() && hosts.contains(&bound) {
                Some(bound)
            } else {
                hosts
                    .iter()
                    .find(|host| {
                        !host
                            .parse::<std::net::IpAddr>()
                            .is_ok_and(|ip| ip.is_unspecified())
                    })
                    .cloned()
            }
        }
        TlsSource::Managed { domain } => Some(domain.clone()),
    };
    let Some(host) = host else {
        return Ok(None);
    };
    if host
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_unspecified())
    {
        return Ok(None);
    }
    let authority = if host.contains(':') {
        format!("[{host}]:{}", addr.port())
    } else {
        format!("{host}:{}", addr.port())
    };
    let origin = format!("https://{authority}");
    let uri: hyper::Uri = origin
        .parse()
        .map_err(|_| ServerError::Identity("invalid configured browser locator host".into()))?;
    if uri
        .authority()
        .is_none_or(|value| value.as_str() != authority || value.as_str().contains('@'))
    {
        return Err(ServerError::Identity(
            "invalid configured browser locator host".into(),
        ));
    }
    Ok(Some(if addr.port() == 443 {
        origin.strip_suffix(":443").unwrap_or(&origin).to_owned()
    } else {
        origin
    }))
}

impl Server {
    /// Make `origin` the one browser locators name, and return it shared.
    /// Serve browsers: HTTPS on `config.listen`.
    ///
    /// # Errors
    ///
    /// Returns [`ServerError::Iroh`] (the transport error kind) when the
    /// certificate cannot be loaded or an address cannot be bound.
    pub async fn serve_web(&self, config: WebConfig) -> Result<WebHandle, ServerError> {
        self.serve_web_also_at(config, Vec::new()).await
    }

    /// [`Self::serve_web`] for a server that is reached under more origins
    /// than the one it publishes: `also` are the others (a front that hands
    /// a second name on to the same listener), named at its start by whoever
    /// keeps it. A setup code's pairing link may name one of them as it may
    /// name the published origin; a name the server was not told stays
    /// another server, whatever a request says of itself.
    ///
    /// # Errors
    ///
    /// As [`Self::serve_web`], and when one of `also` is no origin a server
    /// is published at, or stands beside no published origin.
    pub async fn serve_web_also_at(
        &self,
        config: WebConfig,
        also: Vec<String>,
    ) -> Result<WebHandle, ServerError> {
        let parts = self.web_parts(config, also);
        let handle = serve_parts(parts.clone()).await?;
        self.web_ui.started(parts, &handle);
        Ok(handle)
    }

    /// Serve browsers as [`Self::serve_web`] does, unless the device's own setting says
    /// it serves none: its person turned its web UI off, and the listener is then only
    /// remembered, so that turning it on serves it. `None` when it serves none.
    ///
    /// # Errors
    ///
    /// As [`Self::serve_web`].
    pub async fn serve_web_if_wanted(
        &self,
        config: WebConfig,
    ) -> Result<Option<WebHandle>, ServerError> {
        // Its entry among its person's servers says whether it serves one: read before
        // binding, so a server its person turned off opens no port at all.
        if let Some(workspaces) = self.workspaces() {
            let cert = workspaces.device_cert().clone();
            if let Ok(servers) = crate::servers::read(workspaces).await
                && let Some(entry) = servers.active(&cert.device_public_key)
            {
                self.web_ui
                    .set_wanted(entry.web != crate::servers::WEB_SERVED[1]);
            }
        }
        let parts = self.web_parts(config, Vec::new());
        if !self.web_ui.wanted() {
            self.web_ui.ready(parts);
            return Ok(None);
        }
        let handle = serve_parts(parts.clone()).await?;
        self.web_ui.started(parts, &handle);
        Ok(Some(handle))
    }

    /// What a browser listener is served from, as the device's web UI setting serves it
    /// again after it stopped.
    fn web_parts(&self, config: WebConfig, also: Vec<String>) -> WebParts {
        WebParts {
            context: self.web_context().clone(),
            live: self
                .live_origin
                .as_ref()
                .map(crate::web::live::LiveOrigin::served),
            endpoint: self.endpoint(),
            node_addr: self.node_addr(),
            state_root: self.state_root().to_path_buf(),
            workspaces: self.workspaces().cloned(),
            content_links: self.content_links(),
            browser_origin: Arc::clone(&self.browser_origin),
            config,
            also,
        }
    }
}

/// What a browser listener is served from, kept so the device can serve it again after its
/// person stopped it.
#[derive(Clone)]
pub(crate) struct WebParts {
    context: WebContext,
    /// The live origin this server runs, when it runs one.
    live: Option<live::Served>,
    endpoint: iroh::Endpoint,
    node_addr: iroh::EndpointAddr,
    state_root: std::path::PathBuf,
    workspaces: Option<crate::workspaces::Workspaces>,
    content_links: Arc<crate::content_links::ContentLinks>,
    browser_origin: Arc<std::sync::RwLock<Option<Arc<String>>>>,
    config: WebConfig,
    also: Vec<String>,
}

/// Serve a browser listener from `parts`: the same bring-up whoever started it, the
/// embedder's [`Server::serve_web`] or the device's web UI setting serving it again.
#[allow(
    clippy::too_many_lines,
    reason = "one bring-up of every browser-facing part, each started where it reads"
)]
async fn serve_parts(parts: WebParts) -> Result<WebHandle, ServerError> {
    let WebParts {
        context,
        live,
        endpoint,
        node_addr,
        state_root,
        workspaces,
        content_links,
        browser_origin,
        config,
        also,
    } = parts;
    let failed = |message: String| ServerError::Iroh(format!("web listener: {message}"));
    checked_origins(&config, &also).map_err(failed)?;
    if !also.is_empty() {
        tracing::info!(origins = ?also, "this server is also reached under these origins");
    }
    let identity = tls::Identity::load(&config.tls).map_err(failed)?;
    let (tcp, udp) = bind_listeners(&config).map_err(|error| failed(error.to_string()))?;
    let addr = tcp
        .local_addr()
        .map_err(|error| failed(error.to_string()))?;
    let public_origin = match config.public_origin.clone() {
        Some(origin) => Some(origin),
        None => browser_locator_origin(&config.tls, addr)?,
    };
    let acceptor =
        tokio_rustls::TlsAcceptor::from(Arc::new(identity.https_config().map_err(failed)?));
    let fetch = fetch::Serving::start(config.fetch_proxy.as_ref()).map_err(failed)?;
    // A host that serves a proxy mints the token of each grant its pages ask.
    if fetch.proxy.is_some() {
        context.server.fetch_grants.serves();
    }
    let Some(live) = live else {
        return Err(failed("the server runs no live session".into()));
    };
    let server_cert = context.server.identity.cert.clone();
    // Where a page dials this host: its endpoint's key and addresses, and
    // the relay it is reached through — which the endpoint names from the
    // moment this host serves anything ([`Server::start`] waits for it), as
    // a page has no UDP socket and its own endpoint carries no packet
    // without one.
    // `/host` answers the endpoint's address as it is when asked
    // ([`Web::current_host_info`]); this is where it starts.
    let relay_url = node_addr.relay_urls().next().map(ToString::to_string);
    let (link_listener, web_link) =
        bind_link(&config, udp, addr, public_origin.as_ref(), &identity).map_err(failed)?;
    // Where pages reach this host, for whoever holds the server and not this handle: a
    // page dials it as the one its host published.
    let _ = context.link.set(web_link.clone());
    let server_info = ServerInfo {
        server_cert,
        address: Some(ServerAddress {
            endpoint_addr: node_addr,
            relay_url,
            web: Some(web_link.clone()),
        }),
        origin: public_origin.clone(),
    };
    // What a site hands its page about this host, kept in its state (`web-host.json`).
    tilo_files::native::atomic::write_with_mode(
        &state_root.join("web-host.json"),
        &server_info
            .web_host_json()
            .map_err(|error| failed(format!("name this host: {error}")))?,
        0o644,
    )
    .map_err(|error| failed(format!("keep this host's name: {error}")))?;
    let web = Arc::new(Web {
        context: context.clone(),
        server_info: server_info.clone(),
        endpoint: Some(endpoint),
        allowed_origins: config.allowed_origins.clone(),
        workspaces: workspaces.clone(),
        profiles: Arc::new(profiles::Profiles::new(state_root.join("profiles"))),
        fetch: fetch.proxy,
        grants: context.server.fetch_grants.clone(),
        content_links,
        presence: live.authority(),
        page_errors: page_errors::PageErrors::default(),
        site: config.site.as_deref().and_then(Site::new),
        state_root: state_root.clone(),
        served_lists: ServedLists::default(),
    });
    // Each listed workspace's wait list opens now: its entry mails the addresses
    // people leave, and the invitations members ask for, from the start.
    if let Some(workspaces) = &web.workspaces {
        for workspace in workspaces.published_workspaces().await {
            if let Err(error) = web.wait_list(workspaces, &workspace).await {
                tracing::warn!(
                    %workspace,
                    %error,
                    "this workspace's wait list could not be opened"
                );
            }
        }
    }
    // The name each person gives themselves is kept with their profile, which the workspaces
    // here name them by.
    if let Some(workspaces) = &web.workspaces {
        workspaces.person_names(web.profiles.clone());
        // The short links of the invitations the workspaces issue name
        // this host, which answers a page for the invitation a link names
        // (`OPEN_INVITATION_PATH`): the link a member hands over and the
        // one the mail to an address carries are the same.
        if let Some(origin) = &public_origin {
            let _ = workspaces.invitations_at(origin);
        }
    }
    #[cfg(feature = "agents")]
    web.serve_pushes(
        &state_root,
        config.site.as_deref(),
        public_origin.as_deref(),
        addr,
    );
    let mut tasks = Vec::new();
    tasks.extend(fetch.metrics);
    let mut plain_origin = None;
    if let Some(plain) = config.plain_listen {
        let plain = tokio::net::TcpListener::bind(plain)
            .await
            .map_err(|error| failed(error.to_string()))?;
        let port = plain
            .local_addr()
            .map_err(|error| failed(error.to_string()))?
            .port();
        plain_origin = Some(format!("http://localhost:{port}"));
        tasks.push(tokio::spawn(plain_accept_loop(plain, web.clone())));
    }
    tasks.push(tokio::spawn(link::serve(
        link_listener,
        link::Serving {
            server: context.server.clone(),
            live_origin: live.relay(),
        },
    )));
    tasks.push(tokio::spawn(accept_loop(tcp, acceptor, web)));
    let published_origin = publish_browser_origin(&browser_origin, public_origin.clone())?;
    Ok(WebHandle {
        addr,
        origin: public_origin.unwrap_or_else(|| format!("https://{addr}")),
        certificate_sha256: identity.fingerprint,
        link: web_link,
        plain_origin,
        server_info,
        tasks: Arc::new(WebTasks {
            tasks: std::sync::Mutex::new(tasks),
            locator_origin: browser_origin,
            published_origin,
            stopped: std::sync::atomic::AtomicBool::new(false),
        }),
        presence: live.authority(),
    })
}

/// Make `origin` the one browser locators name, and return it shared.
fn publish_browser_origin(
    browser_origin: &std::sync::RwLock<Option<Arc<String>>>,
    origin: Option<String>,
) -> Result<Option<Arc<String>>, ServerError> {
    let published = origin.map(Arc::new);
    browser_origin
        .write()
        .map_err(|_| ServerError::Iroh("web listener: browser locator lock poisoned".into()))?
        .clone_from(&published);
    Ok(published)
}

impl Server {}

/// Where a page reaches this host: WebTransport on `socket`, at the HTTPS
/// listener's origin and certificate unless the deployment names another way in.
fn bind_link(
    config: &WebConfig,
    socket: std::net::UdpSocket,
    addr: SocketAddr,
    public_origin: Option<&String>,
    identity: &tls::Identity,
) -> Result<(tilo_net::webtransport::Listener, WebLink), String> {
    let bound = socket
        .local_addr()
        .map_err(|error| format!("WebTransport listener: {error}"))?;
    let listener = tilo_net::webtransport::Listener::bind(socket, identity.certificate()?)
        .map_err(|error| format!("WebTransport listener: {error}"))?;
    let link = WebLink {
        origin: named_port(
            &config
                .link
                .as_ref()
                .map(|link| link.origin.clone())
                .or_else(|| public_origin.cloned())
                .unwrap_or_else(|| format!("https://{addr}")),
            bound.port(),
        ),
        certificate_sha256: identity
            .fingerprint
            .as_deref()
            .and_then(|fingerprint| hex::decode(fingerprint).ok())
            .and_then(|bytes| bytes.try_into().ok()),
    };
    Ok((listener, link))
}

/// `origin` naming the port `bound` a listener took. An origin ending in `:0`
/// names none of its own: the kernel picked the port, and this is where it
/// answers. Any other origin is reached exactly as written, at the name and
/// port its deployment publishes.
fn named_port(origin: &str, bound: u16) -> String {
    let Some((scheme, rest)) = origin.split_once("://") else {
        return origin.to_owned();
    };
    let (authority, path) = rest
        .find('/')
        .map_or((rest, ""), |slash| rest.split_at(slash));
    match authority.strip_suffix(":0") {
        Some(name) => format!("{scheme}://{name}:{bound}{path}"),
        None => origin.to_owned(),
    }
}

/// How many kernel-picked ports [`bind_listeners`] tries before it gives up.
const PORT_PAIR_ATTEMPTS: usize = 64;

/// The HTTPS listener on `config.listen` and the UDP socket WebTransport takes
/// sessions on: beside it on the same port unless [`WebConfig::link`] names
/// its own. For port 0 the kernel picks a UDP port and the HTTPS listener takes
/// the same one, so the pair is held from the start; a port whose TCP side is
/// in use is passed over for the next one the kernel picks.
fn bind_listeners(
    config: &WebConfig,
) -> std::io::Result<(tokio::net::TcpListener, std::net::UdpSocket)> {
    let listen = config.listen;
    if let Some(link) = &config.link {
        return Ok((
            bind_listener(listen)?,
            std::net::UdpSocket::bind(link.listen)?,
        ));
    }
    if listen.port() != 0 {
        return Ok((bind_listener(listen)?, std::net::UdpSocket::bind(listen)?));
    }
    let mut last = None;
    for _ in 0..PORT_PAIR_ATTEMPTS {
        let udp = std::net::UdpSocket::bind(listen)?;
        let paired = SocketAddr::new(listen.ip(), udp.local_addr()?.port());
        match bind_listener(paired) {
            Ok(tcp) => return Ok((tcp, udp)),
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => last = Some(error),
            Err(error) => return Err(error),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::from(std::io::ErrorKind::AddrInUse)))
}

/// The HTTPS listener on `listen`.
fn bind_listener(listen: SocketAddr) -> std::io::Result<tokio::net::TcpListener> {
    let socket = if listen.is_ipv4() {
        tokio::net::TcpSocket::new_v4()?
    } else {
        tokio::net::TcpSocket::new_v6()?
    };
    // As `TcpListener::bind`: a restarted host takes its port back while its
    // old connections linger in TIME_WAIT.
    socket.set_reuseaddr(true)?;
    socket.bind(listen)?;
    socket.listen(1024)
}

/// Accepted connections belong to their listener's loop: aborting the loop
/// (dropping the [`WebHandle`]) closes every open connection with it, so a
/// browser sees its host go away instead of a kept-alive orphan serving it.
async fn accept_loop(
    tcp: tokio::net::TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
    web: Arc<Web>,
) {
    let mut connections = tokio::task::JoinSet::new();
    loop {
        let Ok((stream, peer)) = tcp.accept().await else {
            continue;
        };
        while connections.try_join_next().is_some() {}
        let acceptor = acceptor.clone();
        let web = web.clone();
        connections.spawn(async move {
            let Ok(Ok(tls)) =
                tokio::time::timeout(Duration::from_secs(10), acceptor.accept(stream)).await
            else {
                return;
            };
            serve_connection(tls, peer, web).await;
        });
    }
}

/// The same routes as plain HTTP on a loopback listener.
async fn plain_accept_loop(tcp: tokio::net::TcpListener, web: Arc<Web>) {
    let mut connections = tokio::task::JoinSet::new();
    loop {
        let Ok((stream, peer)) = tcp.accept().await else {
            continue;
        };
        while connections.try_join_next().is_some() {}
        connections.spawn(serve_connection(stream, peer, web.clone()));
    }
}

/// The address a request's connection came from.
#[derive(Clone, Copy, Debug)]
struct Peer(std::net::IpAddr);

async fn serve_connection<Io>(io: Io, peer: SocketAddr, web: Arc<Web>)
where
    Io: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let service = hyper::service::service_fn(move |mut request: Request<hyper::body::Incoming>| {
        let web = web.clone();
        request.extensions_mut().insert(Peer(peer.ip()));
        async move { Ok::<_, Infallible>(handle(&web, request).await) }
    });
    let _ = hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new())
        .serve_connection(hyper_util::rt::TokioIo::new(io), service)
        .await;
}

/// A response body: whole bytes, what the fetch proxy relays, or what is
/// read for it as it is sent (a link to a workspace's files).
pub(crate) enum Body {
    Full(Option<Bytes>),
    Proxied(tilo_net::proxy::host::ProxyBody),
    Streamed(tokio::sync::mpsc::Receiver<Result<Bytes, String>>),
}

/// Why a body broke off: the browser then sees an error rather than a short
/// body that reads as whole.
#[derive(Debug)]
pub(crate) struct BodyError(String);

impl std::fmt::Display for BodyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BodyError {}

impl http_body::Body for Body {
    type Data = Bytes;
    /// A relayed body breaks off at the proxy's bounds; a streamed one where
    /// its bytes could not be read.
    type Error = BodyError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
        match self.get_mut() {
            Self::Full(bytes) => {
                Poll::Ready(bytes.take().map(|bytes| Ok(http_body::Frame::data(bytes))))
            }
            Self::Proxied(body) => Pin::new(body)
                .poll_frame(cx)
                .map_err(|error| BodyError(error.to_string())),
            Self::Streamed(pieces) => pieces.poll_recv(cx).map(|piece| {
                piece.map(|piece| piece.map(http_body::Frame::data).map_err(BodyError))
            }),
        }
    }
}

fn reply(status: StatusCode, body: Vec<u8>) -> Response<Body> {
    let mut response = Response::new(Body::Full(Some(Bytes::from(body))));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/octet-stream"),
    );
    response
}

fn encoded<T: serde::Serialize>(value: &T) -> Response<Body> {
    match postcard::to_allocvec(value) {
        Ok(bytes) => reply(StatusCode::OK, bytes),
        Err(_) => reply(StatusCode::INTERNAL_SERVER_ERROR, Vec::new()),
    }
}

fn status(status: StatusCode, message: &str) -> Response<Body> {
    reply(status, message.as_bytes().to_vec())
}

/// The picture a public avatar `token` names, as the image it is, or nothing
/// once its person replaced or removed it.
fn avatar(web: &Web, token: &str) -> Response<Body> {
    let Some(bytes) = web.profiles.photo_for_token(token) else {
        return status(StatusCode::NOT_FOUND, "no picture at this address");
    };
    let Some(kind) = tilo_server_protocol::public_avatars::sniff(&bytes) else {
        return status(StatusCode::NOT_FOUND, "no picture at this address");
    };
    let mut response = reply(StatusCode::OK, bytes);
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, header::HeaderValue::from_static(kind));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    // An address names one picture for good: a replaced picture is another
    // address, so nothing may keep this one past a change.
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

/// Whether the request asks for a page a person reads, as a browser opening an
/// address does, rather than for a body a caller takes as data.
pub(super) fn wants_a_page(request: &Request<hyper::body::Incoming>) -> bool {
    request
        .headers()
        .get(header::ACCEPT)
        .and_then(|accept| accept.to_str().ok())
        .is_some_and(|accept| accept.contains("text/html"))
}

/// The answer to a browser that opened the host's own address: on to the page
/// that can take it, a redirect the browser follows by itself.
pub(super) fn redirect_to(location: &str) -> Response<Body> {
    let mut response = reply(StatusCode::FOUND, Vec::new());
    if let Ok(location) = header::HeaderValue::from_str(location) {
        response.headers_mut().insert(header::LOCATION, location);
    }
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

/// Where a browser that asked this host for a page it does not serve goes:
/// the site the host belongs to, when it knows one.
fn site_for_a_page(web: &Web, request: &Request<hyper::body::Incoming>) -> Option<Response<Body>> {
    if request.method() != Method::GET || !wants_a_page(request) {
        return None;
    }
    let site = web.site.as_ref()?;
    (!site.served_it(request)).then(|| redirect_to(&site.origin))
}

fn json_status<T: serde::Serialize>(code: StatusCode, value: &T) -> Response<Body> {
    match serde_json::to_vec(value) {
        Ok(json) => {
            let mut response = reply(code, json);
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("application/json"),
            );
            response
        }
        Err(_) => reply(StatusCode::INTERNAL_SERVER_ERROR, Vec::new()),
    }
}

fn json<T: serde::Serialize>(value: &T) -> Response<Body> {
    json_status(StatusCode::OK, value)
}

/// A public workspace's entry (`/directory/<workspace>/report` or `…/errors`,
/// and its wait list): JSON in, JSON out, a refusal as `{error}` the person
/// reads. A person is let in through it on their device's session
/// ([`tilo_server_protocol::entering`]), not here.
#[allow(
    clippy::result_large_err,
    reason = "the error is the response the handler sends as it is"
)]
async fn directory_entry(
    web: &Web,
    route: &str,
    bytes: &[u8],
    client: std::net::IpAddr,
) -> Result<Response<Body>, Response<Body>> {
    use tilo_server_protocol::directory::{
        EntryReport, WAIT_LIST, WAIT_LIST_CONFIRM, WAIT_LIST_REMOVE, WAIT_LIST_STATUS, entry_route,
    };
    let (workspace, step) =
        entry_route(route).ok_or_else(|| status(StatusCode::NOT_FOUND, "not found"))?;
    // A server that hosts no workspaces refuses before the request is parsed.
    let workspaces = served_workspaces(web)?;
    // The wait list a workspace taking people by invitation keeps: an address
    // left, confirmed, taken off, and where it stands.
    if matches!(
        step,
        WAIT_LIST | WAIT_LIST_REMOVE | WAIT_LIST_CONFIRM | WAIT_LIST_STATUS
    ) {
        let wait_list = match web.wait_list(workspaces, workspace).await {
            Ok(Some(wait_list)) => wait_list,
            Ok(None) => {
                return Err(refused(
                    StatusCode::NOT_FOUND,
                    "this workspace keeps no wait list",
                ));
            }
            Err(error) => {
                tracing::error!(
                    %workspace,
                    %error,
                    "the workspace's wait list could not be served"
                );
                return Err(status(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "this workspace cannot serve its wait list",
                ));
            }
        };
        let answered = crate::entry::route(&wait_list, workspaces, client, step, bytes).await;
        let mut response = json_status(answered.status, &answered.body);
        if let Some(seconds) = answered.retry_after_secs
            && let Ok(value) = header::HeaderValue::from_str(&seconds.to_string())
        {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        return Ok(response);
    }
    let refused = |error: crate::workspaces::CollaborationError| {
        json_status(
            StatusCode::FORBIDDEN,
            &serde_json::json!({ "error": error.to_string() }),
        )
    };
    match step {
        "report" => {
            let report: EntryReport = serde_json::from_slice(bytes)
                .map_err(|_| status(StatusCode::BAD_REQUEST, "not a report"))?;
            workspaces
                .report_listed(workspace, &report)
                .await
                .map_err(refused)?;
            Ok(reply(StatusCode::NO_CONTENT, Vec::new()))
        }
        // A page's own uncaught error: one log line for the site's operator,
        // and the same answer whatever became of it, so a page never retries.
        "errors" => {
            web.page_errors.report(workspace, bytes);
            Ok(reply(StatusCode::NO_CONTENT, Vec::new()))
        }
        _ => Err(status(StatusCode::NOT_FOUND, "not found")),
    }
}

/// The workspaces this server hosts, or the answer that it hosts none.
#[allow(
    clippy::result_large_err,
    reason = "the error is the response the handler sends as it is"
)]
fn served_workspaces(web: &Web) -> Result<&crate::workspaces::Workspaces, Response<Body>> {
    web.workspaces
        .as_ref()
        .ok_or_else(|| status(StatusCode::NOT_FOUND, "this server hosts no workspaces"))
}

#[allow(
    clippy::result_large_err,
    reason = "the error is the response the handler sends as it is"
)]
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, Response<Body>> {
    postcard::from_bytes(bytes).map_err(|_| status(StatusCode::BAD_REQUEST, "malformed body"))
}

/// Headers a page may send to the browser routes.
const ALLOWED_HEADERS: &str = "content-type, x-tilo-session";

/// Whether `path` is part of the public directory: a listing anyone may read,
/// and an entry anyone may ask, from any site.
fn public_directory_route(path: &str) -> bool {
    directory_route(path).is_some()
}

/// The public directory's route a site names, below the listener's own paths.
///
/// A server's own routes live below [`API_PREFIX`]. A site names its directory
/// in its `entry.json` and hangs each entry off that address
/// ([`tilo_server_protocol::directory`]), so the same routes answer at the
/// site's own paths — `/directory` and `/directory/<workspace>/report` — for a
/// host a site's person reaches without the prefix: the ingress in front of
/// it sends the site's paths to the server.
fn directory_route(path: &str) -> Option<&str> {
    let route = path.strip_prefix(API_PREFIX).unwrap_or(path);
    (route == tilo_server_protocol::directory::DIRECTORY_PATH
        || tilo_server_protocol::directory::entry_route(route).is_some())
    .then_some(route)
}

/// CORS for the public directory: any origin, no credentials, and the
/// `Retry-After` a full entry answers with. A page on any site reads the
/// listings and reports a workspace without going through its host.
fn public_cors(response: &mut Response<Body>) {
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        header::HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        header::HeaderValue::from_static("POST, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        header::HeaderValue::from_static("content-type"),
    );
    headers.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        header::HeaderValue::from_static("retry-after"),
    );
}

fn cors(
    allowed_origins: &[String],
    allowed_headers: &str,
    origin: Option<&header::HeaderValue>,
    response: &mut Response<Body>,
) {
    let Some(origin) = origin.and_then(|value| value.to_str().ok()) else {
        return;
    };
    if !allowed_origins
        .iter()
        .any(|allowed| allowed == origin || allowed == "*")
    {
        return;
    }
    let headers = response.headers_mut();
    if let Ok(origin) = header::HeaderValue::from_str(origin) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    }
    headers.insert(header::VARY, header::HeaderValue::from_static("origin"));
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        header::HeaderValue::from_static("POST, OPTIONS"),
    );
    if let Ok(allowed) = header::HeaderValue::from_str(allowed_headers) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, allowed);
    }
}

async fn handle(web: &Web, request: Request<hyper::body::Incoming>) -> Response<Body> {
    // The deployment's readiness probe: what its ingress asks before a page is
    // reached, and what an operator's uptime check reads. It is a GET, which
    // every other route here past the pages refuses.
    if request.uri().path() == "/mode" {
        if request.method() != Method::GET {
            return status(StatusCode::METHOD_NOT_ALLOWED, "GET");
        }
        return json(&serde_json::json!({
            "server": "up",
            "revision": std::env::var("TILO_SOURCE_REVISION").unwrap_or_default(),
        }));
    }
    // The fetch proxy takes the method of the request it relays.
    if request
        .uri()
        .path()
        .strip_prefix(API_PREFIX)
        .is_some_and(|route| route == protocol::FETCH_PATH)
    {
        return fetch::handle(web, request).await;
    }
    // A link to a workspace's files is fetched by the browser itself.
    if let Some(link) = request
        .uri()
        .path()
        .strip_prefix(API_PREFIX)
        .and_then(|route| route.strip_prefix(crate::content_links::ROUTE))
    {
        let link = link.to_owned();
        let view = request
            .uri()
            .query()
            .is_some_and(|query| query.split('&').any(|pair| pair == "view"));
        let origin = request.headers().get(header::ORIGIN).cloned();
        let mut response = content::handle(web, request.method(), &link, view).await;
        cors(
            &web.allowed_origins,
            ALLOWED_HEADERS,
            origin.as_ref(),
            &mut response,
        );
        return response;
    }
    // The page's face, which the page's stylesheet names on this origin.
    if request.method() == Method::GET
        && let Some(response) = page::font(request.uri().path())
    {
        return response;
    }
    // A workspace's app as a web page: the document its script opens the
    // page from, over the device's own session.
    if request.method() == Method::GET
        && let Some(address) = tilo_server_protocol::page::PageAddress::parse(request.uri().path())
    {
        let link = web
            .server_info
            .address
            .as_ref()
            .and_then(|address| address.web.as_ref());
        return page::answer(&address, link, web.server_info.origin.as_deref());
    }
    // A person's picture, which a place outside Tilo fetches at the public
    // address a bridged post names.
    if request.method() == Method::GET
        && let Some(token) = request
            .uri()
            .path()
            .strip_prefix(tilo_server_protocol::public_avatars::ROUTE)
    {
        return avatar(web, token);
    }
    let origin = request.headers().get(header::ORIGIN).cloned();
    let public = public_directory_route(request.uri().path());
    let mut response = if request.method() == Method::OPTIONS {
        reply(StatusCode::NO_CONTENT, Vec::new())
    } else if request.method() != Method::POST {
        // A browser that opened an address here asks for a page, not for a
        // text body: it goes to the site the host belongs to, which has one.
        site_for_a_page(web, &request)
            .unwrap_or_else(|| status(StatusCode::METHOD_NOT_ALLOWED, "POST only"))
    } else {
        route(web, request)
            .await
            .unwrap_or_else(|response| response)
    };
    if public {
        public_cors(&mut response);
    } else {
        cors(
            &web.allowed_origins,
            ALLOWED_HEADERS,
            origin.as_ref(),
            &mut response,
        );
    }
    response
}

async fn body(
    request: Request<hyper::body::Incoming>,
    limit: usize,
) -> Result<(hyper::http::request::Parts, Bytes), Response<Body>> {
    let (parts, body) = request.into_parts();
    let bytes = http_body_util::Limited::new(body, limit)
        .collect()
        .await
        .map_err(|_| status(StatusCode::PAYLOAD_TOO_LARGE, "body too large"))?
        .to_bytes();
    Ok((parts, bytes))
}

/// Whether `user` is admitted to any workspace this server serves: a member
/// here.
async fn admitted_anywhere(web: &Web, user: [u8; 32]) -> bool {
    // The person this host runs for is its own before any workspace is bound.
    web.context.server.admits(user).await
}

/// The client a request came from: its connection's address, or, for a
/// connection from a proxy in front of this host (a loopback or private
/// address), the last address that proxy names in `X-Forwarded-For`.
fn client_address(request: &Request<hyper::body::Incoming>) -> Option<std::net::IpAddr> {
    let Peer(peer) = *request.extensions().get::<Peer>()?;
    let proxied = match peer {
        std::net::IpAddr::V4(v4) => v4.is_loopback() || v4.is_private(),
        std::net::IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local(),
    };
    let forwarded = proxied
        .then(|| {
            request
                .headers()
                .get_all("x-forwarded-for")
                .iter()
                .next_back()
        })
        .flatten()
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.rsplit(',').next())
        .and_then(|last| last.trim().parse().ok());
    Some(forwarded.unwrap_or(peer))
}

/// A JSON refusal the page reads: `{error}` with `code`.
fn refused(code: StatusCode, message: &str) -> Response<Body> {
    let mut response = json_status(code, &serde_json::json!({ "error": message }));
    if code == StatusCode::TOO_MANY_REQUESTS || code == StatusCode::SERVICE_UNAVAILABLE {
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, header::HeaderValue::from_static("60"));
    }
    response
}

#[allow(
    clippy::too_many_lines,
    reason = "one routing table: every browser route and its body type in one place"
)]
async fn route(
    web: &Web,
    request: Request<hyper::body::Incoming>,
) -> Result<Response<Body>, Response<Body>> {
    let path = request.uri().path().to_owned();
    // A site's own paths for the public directory — `/directory` and each
    // entry below it — reach the same routes as their versions below the
    // prefix, so a host a site's person dials by the address its `entry.json`
    // names answers without being reached under the prefix.
    let Some(route) = path
        .strip_prefix(API_PREFIX)
        .or_else(|| directory_route(&path))
    else {
        return Err(status(StatusCode::NOT_FOUND, "not found"));
    };
    let client = client_address(&request);
    let (_, bytes) = body(request, MAX_REQUEST_BYTES).await?;
    match route {
        "/host" => Ok(encoded(&web.current_host_info())),
        tilo_server_protocol::web::WORKSPACE_DELETED_PATH => {
            let workspace: String = decode(&bytes)?;
            let deleted = web.workspaces.as_ref().is_some_and(|workspaces| {
                WorkspaceId::parse(&workspace)
                    .is_some_and(|workspace_id| workspaces.deleted(workspace_id))
            });
            Ok(encoded(&deleted))
        }
        tilo_server_protocol::web::INVITATION_PATH => {
            let workspaces = web
                .workspaces
                .as_ref()
                .ok_or_else(|| status(StatusCode::NOT_FOUND, "this server hosts no workspaces"))?;
            let invitation: Vec<u8> = decode(&bytes)?;
            Ok(encoded(&workspaces.inspect_invitation(&invitation).await))
        }
        tilo_server_protocol::web::OPEN_INVITATION_PATH => {
            let workspaces = web
                .workspaces
                .as_ref()
                .ok_or_else(|| status(StatusCode::NOT_FOUND, "this server hosts no workspaces"))?;
            let asked: tilo_server_protocol::web::KeptInvitation = serde_json::from_slice(&bytes)
                .map_err(|_| {
                status(
                    StatusCode::BAD_REQUEST,
                    "that is not an invitation's secret",
                )
            })?;
            // Spent, lapsed, taken back and never issued read alike: a link
            // tells nothing about the invitations it does not name.
            let signed = workspaces
                .kept_invitation(&asked.workspace, &asked.secret)
                .await
                .ok_or_else(|| status(StatusCode::GONE, "that invitation is gone"))?;
            Ok(json(&tilo_server_protocol::web::OpenedInvitation {
                invitation: tilo_peer_state::join::encode_invitation_text(&signed),
            }))
        }
        tilo_server_protocol::directory::DIRECTORY_PATH => {
            let workspaces = web
                .workspaces
                .as_ref()
                .ok_or_else(|| status(StatusCode::NOT_FOUND, "this server hosts no workspaces"))?;
            // The operator's kill switch: a host paused entirely serves its listing to nobody,
            // while one kept watch-only still lets a page read who is here. Either way the
            // people already in it work on.
            let mode = workspaces.mode();
            if !mode.allows_spectators() {
                return Err(refused(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "the host is paused; back soon",
                ));
            }
            let online = web.presence.devices().into_keys().collect();
            Ok(json(&tilo_server_protocol::directory::DirectoryPage {
                host: Some(web.current_host_info().for_site()),
                workspaces: workspaces.directory(&online).await,
            }))
        }
        _ if tilo_server_protocol::directory::entry_route(route).is_some() => {
            let client = client.unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
            directory_entry(web, route, &bytes, client).await
        }
        "/profile" => {
            let now_ms = tilo_clock::unix_now_ms();
            web.profiles
                .publish(&bytes, now_ms, |root| admitted_anywhere(web, root))
                .await
                .map(|root| {
                    // A profile is what every surface showing them as an author reads: the
                    // members' pictures moved with it, and the apps' member lists read them
                    // again.
                    tilo_identity::policy_change::announce();
                    // A profile's name is the person's in every workspace here: each names them
                    // by it from now on.
                    let kept = (web.workspaces.clone(), web.profiles.kept_name(&root));
                    if let (Some(workspaces), Some(name)) = kept {
                        tokio::spawn(async move {
                            workspaces
                                .name_person_everywhere(root, &name, tilo_clock::unix_now_ns())
                                .await;
                        });
                    }
                    reply(StatusCode::NO_CONTENT, Vec::new())
                })
                .map_err(|refusal| {
                    let code = match refusal {
                        profiles::Refusal::Malformed(_) => StatusCode::BAD_REQUEST,
                        profiles::Refusal::BadSignature | profiles::Refusal::NotMember => {
                            StatusCode::FORBIDDEN
                        }
                        profiles::Refusal::Stale => StatusCode::CONFLICT,
                        profiles::Refusal::Storage => StatusCode::INTERNAL_SERVER_ERROR,
                    };
                    status(code, refusal.message())
                })
        }
        "/profile/fetch" => match web.profiles.answer(&bytes) {
            Some(json) => {
                let mut response = reply(StatusCode::OK, json);
                response.headers_mut().insert(
                    header::CONTENT_TYPE,
                    header::HeaderValue::from_static("application/json"),
                );
                Ok(response)
            }
            None => Err(status(StatusCode::BAD_REQUEST, "malformed root public key")),
        },
        _ => Err(status(StatusCode::NOT_FOUND, "not found")),
    }
}
