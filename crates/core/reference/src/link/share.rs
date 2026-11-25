//! The link a resource's `share-link` mints: one rendering of the resource's address under the
//! web entry the workspace's host announces.
//!
//! The host asks its own catalogue for the locator every time, so a link never names an entry or
//! a workspace the host no longer serves, and is refused where the host serves no page rather than
//! guessing one. It is rendered here, where the grammar is, because an app's own root mints it
//! and so does a tilolang app's host: one grammar, one refusal.
//!
//! A resource says what its link names in a `ShareRequest`
//! ([`tilo_resource::share::ShareRequest`]); this module reads it, so a caller never spells the
//! grammar itself.

use serde::{Deserialize, Serialize};
use tilo_actions::remote::ResourceViewLocator;
use tilo_resource::share::ShareRequest;

/// Where a workspace's resources open on the web: the entry a link is rendered under, the host's
/// origin and the workspace's id, as the workspace's own host announces them. What a session
/// lends a host and a device's daemon hands its apps, for the links their resources mint under.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkHome {
    /// The host's origin (`https://eu1.tilo.cloud`).
    pub origin: String,
    /// The workspace the resources are kept in.
    pub workspace: String,
    /// The certified host device, as its key in hexadecimal.
    #[serde(default)]
    pub host_device: String,
    /// The page base (`https://tilo.so/`) links are rendered under, ending in `/`.
    pub web_entry: String,
}

impl LinkHome {
    /// The home as the JSON a host's process receives with a window's instance
    /// (`runtime_portal::process_link::InstanceRequest::link_home`).
    ///
    /// # Errors
    ///
    /// When it cannot be written as JSON.
    pub fn json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|error| error.to_string())
    }

    /// The locator a link's page is checked against, for the provider `provider` names.
    #[must_use]
    pub fn locator(&self, provider: String) -> ResourceViewLocator {
        ResourceViewLocator {
            origin: self.origin.clone(),
            workspace: self.workspace.clone(),
            host_device: self.host_device.clone(),
            provider,
            web_entry: Some(self.web_entry.clone()),
        }
    }
}

/// The `record:<app>:<record>` provider one record of `app` is served under, the record's primary
/// one when `record` is empty: what a catalogue lookup names the link's page by, and what a locator
/// must answer for the link to be minted.
///
/// # Errors
///
/// The app id or the record id a link cannot carry.
pub fn provider(app: &str, record: &str) -> Result<String, String> {
    let app = crate::link::AppId::parse(app).map_err(|error| error.to_string())?;
    let record = crate::link::ResourceSegment::new(record).map_err(|error| error.to_string())?;
    Ok(format!(
        "record:{}:{}",
        app.as_str(),
        record.provider_part()
    ))
}

/// Why a link is refused when the host that serves the resource announces no page.
pub const NO_WEB_ENTRY: &str =
    "This workspace has no server that serves links; add one in Settings";

/// The link to the resource `request` names, minted under the entry `locator`
/// announces: what a host that answers `share-link` itself renders, and what an
/// app's own root renders from the locator the workspace's host gives it.
///
/// `record` is the record the resource lives in, empty for its app's primary
/// one, and `root` the id the record's root is published under: a link to the
/// root names the record, which opens in its app.
///
/// # Errors
///
/// Why no link can be made: what it asks for does not fit the resource's views,
/// or the locator names another record or serves no page.
pub fn link(
    request: &ShareRequest,
    app: &str,
    record: &str,
    root: &str,
    locator: &ResourceViewLocator,
) -> Result<String, String> {
    render_at(&minting(request, app, record, root)?, locator)
}

/// What a link is minted for: the record's provider, its app and record, the
/// resource it names when it names one, its view and mode.
struct Minting {
    provider: String,
    app: crate::link::AppId,
    record: crate::link::ResourceSegment,
    resource: Option<crate::link::ResourceAddress>,
    view: Option<String>,
    mode: crate::link::Mode,
}

/// What `request` links to, from the record `app` keeps at `record`.
fn minting(request: &ShareRequest, app: &str, record: &str, root: &str) -> Result<Minting, String> {
    use crate::link::{AppId, Mode, ResourceAddress, ResourceSegment};
    let app = AppId::parse(app).map_err(|error| error.to_string())?;
    let record = ResourceSegment::new(record).map_err(|error| error.to_string())?;
    let provider = provider(app.as_str(), record.provider_part())?;
    let args = &request.args;
    let text = |name: &str| args.get(name).and_then(serde_json::Value::as_str);
    let view = match text("view") {
        Some(id) => Some(
            request
                .views
                .iter()
                .find(|view| view.id == id)
                .ok_or_else(|| format!("This has no view `{id}`"))?,
        ),
        None => request
            .views
            .iter()
            .find(|view| view.primary)
            .or_else(|| request.views.first()),
    };
    // Without a view the link names the record alone, which opens in its
    // app: only a record's root offers the link so ([`offer_resource`]).
    let Some(view) = view else {
        let mode = match text("mode").unwrap_or("app") {
            "app" => Mode::App,
            "desktop" => Mode::Desktop,
            other => return Err(format!("Unknown mode `{other}`")),
        };
        if args.get("start").is_some_and(|value| !value.is_null()) {
            return Err("A record's link opens no range".into());
        }
        return Ok(Minting {
            provider,
            app,
            record,
            resource: None,
            view: None,
            mode,
        });
    };
    let offset = |name: &str| -> Result<Option<usize>, String> {
        args.get(name)
            .filter(|value| !value.is_null())
            .map(|value| {
                value
                    .as_u64()
                    .and_then(|offset| usize::try_from(offset).ok())
                    .ok_or_else(|| format!("{name} is not a position in the text"))
            })
            .transpose()
    };
    let span = match (offset("start")?, offset("end")?) {
        (None, None) => None,
        (Some(start), Some(end)) => {
            if !view.range {
                return Err(format!("The view `{}` does not open at a range", view.id));
            }
            let unit = match text("unit").unwrap_or("chars") {
                "chars" => crate::SpanUnit::Chars,
                "bytes" => crate::SpanUnit::Bytes,
                other => return Err(format!("Unknown unit `{other}`")),
            };
            Some(crate::Span::new(start, end, unit))
        }
        _ => return Err("A range needs both start and end".into()),
    };
    let resource = (request.resource != root || span.is_some()).then(|| ResourceAddress {
        kind: request.kind.clone(),
        id: request.resource.clone(),
        span,
    });
    // A view that opens on the whole desktop is linked there; any other
    // opens its app alone full tab. The caller may ask for either.
    let mode = match text("mode").unwrap_or(if view.desktop { "desktop" } else { "app" }) {
        "app" => Mode::App,
        "desktop" => Mode::Desktop,
        other => return Err(format!("Unknown mode `{other}`")),
    };
    Ok(Minting {
        provider,
        app,
        record,
        resource,
        view: (!view.primary).then(|| view.id.clone()),
        mode,
    })
}

/// The link `minting` names under the web entry the host announced on `locator`.
fn render_at(minting: &Minting, locator: &ResourceViewLocator) -> Result<String, String> {
    use crate::link::{HostOrigin, Link, LinkSecrets, LinkTarget, Scope, WebEntry};
    if locator.provider != minting.provider {
        return Err("The workspace's host answered for another record".into());
    }
    let entry = locator.web_entry.as_deref().ok_or(NO_WEB_ENTRY)?;
    let entry = WebEntry::new(entry).map_err(|error| error.to_string())?;
    let workspace = locator
        .workspace
        .parse()
        .map_err(|_| "The workspace's host named no workspace a link can carry")?;
    let host = HostOrigin::new(&locator.origin).map_err(|error| error.to_string())?;
    let target = LinkTarget {
        scope: Scope::Workspace {
            host,
            id: workspace,
            slug: None,
        },
        app: Some(minting.app.clone()),
        record: Some(minting.record.clone()),
        resource: minting.resource.clone(),
        view: minting.view.clone(),
        discuss: None,
    };
    Link {
        mode: minting.mode,
        target,
        secrets: LinkSecrets::default(),
    }
    .to_url(&entry)
    .map_err(|error| error.to_string())
}
