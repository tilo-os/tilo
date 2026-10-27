//! Answers `share-link` and `share-invite-link` ([`tilo_resource::share`]).
//!
//! A link is a rendering of the address the action model already uses, under
//! the web entry the room's host announces. The host is asked for a fresh
//! locator every time, so a link never names an entry or a room the host no
//! longer serves, and is refused when the host serves no web page rather than
//! guessing one. See `docs/architecture/web-routing.md` §4.3.

use tilo_actions::remote::{CatalogueQuery, ViewResourceQuery};
use tilo_reference::link::{
    AppId, HostOrigin, Link, LinkSecrets, LinkTarget, Mode, RecordSegment, ResourceAddress, Scope,
    WebEntry,
};
use tilo_resource::share::{SHARE_INVITE_TYPE, SHARE_TYPE, ShareRequest};
use ui_core::time::{Duration, Instant};
use ui_core::{Arrival, CatalogueCall, RemoteActions};

/// How long a minting waits for the host's locator.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a host's answer cannot make a link.
pub(crate) const NO_WEB_ENTRY: &str = "This workspace's host serves no web page";

/// The record a root publishes from, as a provider names it.
pub(crate) struct Publisher<'a> {
    pub(crate) app: &'a str,
    pub(crate) record: Option<&'a str>,
    /// The root resource: a link to it names the record, not a resource.
    pub(crate) root: &'a str,
}

/// What a link is minted for once the locator arrives.
struct Minting {
    provider: String,
    app: AppId,
    record: RecordSegment,
    resource: Option<ResourceAddress>,
    view: Option<String>,
    mode: Mode,
}

struct Pending {
    invocation: tilo_actions::InvocationId,
    minting: Minting,
    call: CatalogueCall,
    started: Instant,
    /// A person asked through a view: the URL goes to the clipboard.
    copy: bool,
    /// The minted URL, once its copy is under way: the answer waits for the
    /// clipboard's outcome, which settles without blocking the app.
    copying: Option<(String, ui_core::clipboard::ClipboardCopy)>,
}

/// The root's minting of links: its own client for the session's action
/// door, and the mintings waiting on it.
pub(crate) struct ShareLinks {
    remote: RemoteActions,
    pending: Vec<Pending>,
    /// Resources a view asked to copy a link to, in the order asked.
    copy_requested: Vec<String>,
}

impl Default for ShareLinks {
    fn default() -> Self {
        Self {
            remote: RemoteActions::current(),
            pending: Vec::new(),
            copy_requested: Vec::new(),
        }
    }
}

pub(crate) fn handles(type_id: &str) -> bool {
    type_id == SHARE_TYPE || type_id == SHARE_INVITE_TYPE
}

/// What the minting answered and whether a person is to be told.
pub(crate) struct Minted {
    pub(crate) output: tilo_actions::ActionOutput,
    pub(crate) copied: bool,
}

impl ShareLinks {
    pub(crate) fn remote_mut(&mut self) -> &mut RemoteActions {
        &mut self.remote
    }

    /// A view's control is about to invoke a link operation on `resource`:
    /// its URL is copied, not only answered.
    pub(crate) fn note_view_invocation(&mut self, invocation: &tilo_actions::ControlInvocation) {
        if invocation.address.action == tilo_resource::share::SHARE_LINK
            || invocation.address.action == tilo_resource::share::SHARE_INVITE_LINK
        {
            self.copy_requested
                .push(invocation.address.resource.clone());
        }
    }

    /// Start minting the link `payload` asks for.
    ///
    /// # Errors
    /// Why no link can be made: bad arguments, no invite grant, or an
    /// invitation this device cannot create.
    pub(crate) fn start(
        &mut self,
        invocation: tilo_actions::InvocationId,
        payload: &[u8],
        publisher: &Publisher<'_>,
        may_invite: bool,
    ) -> Result<Option<Minted>, String> {
        let request: ShareRequest =
            tilo_actions::decode_json(payload).map_err(|error| error.to_string())?;
        let copy = self
            .copy_requested
            .iter()
            .position(|resource| *resource == request.resource)
            .map(|at| self.copy_requested.remove(at))
            .is_some();
        if request.invite {
            if !may_invite {
                return Err("You may not invite people to this workspace".into());
            }
            // Invitations are created through the room's membership door,
            // which only the shell reaches; an app cannot create one itself.
            return Err(
                "Invitations are created from the workspace's members list, not from an app".into(),
            );
        }
        let minting = minting(&request, publisher)?;
        let call = self.remote.catalogue_page(CatalogueQuery {
            provider: Some(minting.provider.clone()),
            view_resource: Some(ViewResourceQuery {
                id: request.resource,
                kind: request.kind,
            }),
            ..CatalogueQuery::default()
        });
        let mut pending = Pending {
            invocation,
            minting,
            call,
            started: ui_core::time::now(),
            copy,
            copying: None,
        };
        // A host in this process answers at once.
        if let Some(result) = settled(&pending) {
            let url = result?;
            if !pending.copy {
                return Ok(Some(finish(&url, None)));
            }
            let copy = ui_core::try_clipboard_copy(&url);
            if let Some(copied) = copy.outcome() {
                return Ok(Some(finish(&url, Some(copied))));
            }
            pending.copying = Some((url, copy));
        }
        self.pending.push(pending);
        Ok(None)
    }

    /// The update of a minting this answers, once it has one.
    pub(crate) fn poll(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
    ) -> Option<(tilo_actions::InvocationUpdate, bool)> {
        let at = self
            .pending
            .iter()
            .position(|pending| pending.invocation == invocation)?;
        let pending = self.pending.get_mut(at)?;
        if cancellation_requested {
            self.pending.remove(at);
            return Some((
                tilo_actions::InvocationUpdate::Cancelled {
                    invocation,
                    message: None,
                },
                false,
            ));
        }
        if pending.copying.is_none() {
            let result = settled(pending).or_else(|| {
                (ui_core::time::now().saturating_duration_since(pending.started) >= LOOKUP_TIMEOUT)
                    .then(|| Err("The workspace's host did not answer".to_owned()))
            })?;
            match result {
                Ok(url) if pending.copy => {
                    let copy = ui_core::try_clipboard_copy(&url);
                    pending.copying = Some((url, copy));
                }
                Ok(url) => {
                    self.pending.remove(at);
                    return Some(succeeded(invocation, finish(&url, None)));
                }
                Err(message) => {
                    self.pending.remove(at);
                    return Some((
                        tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
                                code: "share_link".into(),
                                message,
                                details: None,
                                retryable: true,
                            },
                        },
                        false,
                    ));
                }
            }
        }
        let (url, copy) = pending.copying.as_ref()?;
        let minted = finish(url, Some(copy.outcome()?));
        self.pending.remove(at);
        Some(succeeded(invocation, minted))
    }
}

/// The URL `pending`'s host answer makes, once it has answered.
fn settled(pending: &Pending) -> Option<Result<String, String>> {
    pending.call.with(|call| match call {
        Arrival::Pending { .. } => None,
        Arrival::Failed(error) => Some(Err(format!(
            "The workspace's host would not look the resource up: {error}"
        ))),
        Arrival::Ready(page) => Some(render(&pending.minting, page)),
    })
}

fn succeeded(
    invocation: tilo_actions::InvocationId,
    minted: Minted,
) -> (tilo_actions::InvocationUpdate, bool) {
    (
        tilo_actions::InvocationUpdate::Succeeded {
            invocation,
            output: minted.output,
        },
        minted.copied,
    )
}

/// The output for `url`, with whether it was copied when a person asked
/// through a view.
fn finish(url: &str, copied: Option<bool>) -> Minted {
    Minted {
        output: tilo_actions::ActionOutput {
            value: Some(match copied {
                Some(copied) => serde_json::json!({ "url": url, "copied": copied }),
                None => serde_json::json!({ "url": url }),
            }),
            resources: Vec::new(),
        },
        copied: copied.unwrap_or(false),
    }
}

/// What `request` links to, from the record `publisher` publishes.
fn minting(request: &ShareRequest, publisher: &Publisher<'_>) -> Result<Minting, String> {
    let app = AppId::new(publisher.app).map_err(|error| error.to_string())?;
    let record =
        RecordSegment::new(publisher.record.unwrap_or("")).map_err(|error| error.to_string())?;
    let provider = format!("record:{}:{}", app.as_str(), record.provider_part());
    let args = &request.args;
    let text = |name: &str| args.get(name).and_then(serde_json::Value::as_str);
    let view = match text("view") {
        Some(id) => request
            .views
            .iter()
            .find(|view| view.id == id)
            .ok_or_else(|| format!("This has no view `{id}`"))?,
        None => request
            .views
            .iter()
            .find(|view| view.primary)
            .or_else(|| request.views.first())
            .ok_or("This has no view to link to")?,
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
                "chars" => tilo_reference::SpanUnit::Chars,
                "bytes" => tilo_reference::SpanUnit::Bytes,
                other => return Err(format!("Unknown unit `{other}`")),
            };
            Some(tilo_reference::Span::new(start, end, unit))
        }
        _ => return Err("A range needs both start and end".into()),
    };
    let resource =
        (request.resource != publisher.root || span.is_some()).then(|| ResourceAddress {
            kind: request.kind.clone(),
            id: request.resource.clone(),
            span,
        });
    let mode = match text("mode").unwrap_or("app") {
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

/// The link `minting` names under the web entry the host announced on `page`.
fn render(minting: &Minting, page: &tilo_actions::remote::CataloguePage) -> Result<String, String> {
    let locator = page
        .locator
        .as_ref()
        .ok_or("The workspace's host did not say where the resource is")?;
    render_at(minting, locator)
}

/// The link to the resource `request` names in `app`'s `record`, for a
/// room's host that answers `share-link` itself on the records it runs
/// headless: the link an app's root mints, under the host's own `locator`.
///
/// # Errors
/// Why there is no link: the request's arguments, an invitation (made from
/// the members list), or a host that serves no web page.
pub fn host_link(
    request: &ShareRequest,
    app: &str,
    record: &str,
    locator: &tilo_actions::remote::ResourceViewLocator,
) -> Result<String, String> {
    if request.invite {
        return Err(
            "Invitations are created from the workspace's members list, not from an app".into(),
        );
    }
    // The host does not know which resource is the record's root, so its
    // links always name the resource, which opens the same place.
    let publisher = Publisher {
        app,
        record: Some(record),
        root: "",
    };
    render_at(&minting(request, &publisher)?, locator)
}

fn render_at(
    minting: &Minting,
    locator: &tilo_actions::remote::ResourceViewLocator,
) -> Result<String, String> {
    if locator.provider != minting.provider {
        return Err("The workspace's host answered for another record".into());
    }
    let entry = locator.web_entry.as_deref().ok_or(NO_WEB_ENTRY)?;
    let entry = WebEntry::new(entry).map_err(|error| error.to_string())?;
    let room = locator
        .room
        .parse()
        .map_err(|_| "The workspace's host named no room a link can carry")?;
    let host = HostOrigin::new(&locator.origin).map_err(|error| error.to_string())?;
    let target = LinkTarget {
        scope: Scope::Room {
            host,
            room,
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

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::remote::{CataloguePage, ResourceViewLocator};
    use tilo_resource::share::SharedView;

    const ROOM: &str = "6f1c2b9e-0d4a-4c1e-9b7a-2f3d1e5c8a90";

    fn request(args: &serde_json::Value) -> ShareRequest {
        ShareRequest {
            resource: "chat.message:01JA".into(),
            kind: "chat.message".into(),
            views: vec![
                SharedView {
                    id: "message".into(),
                    primary: true,
                    range: true,
                },
                SharedView {
                    id: "thread".into(),
                    primary: false,
                    range: false,
                },
            ],
            invite: false,
            args: args.as_object().cloned().unwrap_or_default(),
        }
    }

    fn page(entry: Option<&str>) -> CataloguePage {
        CataloguePage {
            locator: Some(ResourceViewLocator {
                origin: "https://quinten.tilo.cloud".into(),
                room: ROOM.into(),
                host_device: "ab".repeat(32),
                provider: "record:so.tilo.chat:".into(),
                web_entry: entry.map(str::to_owned),
            }),
            ..CataloguePage::default()
        }
    }

    const CHAT: Publisher<'static> = Publisher {
        app: "so.tilo.chat",
        record: Some("@tilo:package-primary"),
        root: "chat",
    };

    fn url(args: &serde_json::Value) -> Result<String, String> {
        render(
            &minting(&request(args), &CHAT)?,
            &page(Some("https://tilo.so/")),
        )
    }

    #[test]
    fn links_render_the_grammar_under_the_announced_entry() {
        assert_eq!(
            url(&serde_json::json!({})).unwrap(),
            format!(
                "https://tilo.so/app/#/@quinten.tilo.cloud/w/{ROOM}/so.tilo.chat/-/chat.message:01JA"
            )
        );
        assert_eq!(
            url(&serde_json::json!({"start": 6, "end": 9, "mode": "desktop"})).unwrap(),
            format!(
                "https://tilo.so/#/@quinten.tilo.cloud/w/{ROOM}/so.tilo.chat/-/chat.message:01JA?chars=6-9"
            )
        );
        assert!(
            url(&serde_json::json!({"view": "thread"}))
                .unwrap()
                .ends_with("chat.message:01JA?view=thread")
        );
    }

    #[test]
    fn what_cannot_be_linked_is_refused_with_its_reason() {
        assert!(url(&serde_json::json!({"view": "thread", "start": 1, "end": 2})).is_err());
        assert!(url(&serde_json::json!({"start": 1})).is_err());
        assert!(url(&serde_json::json!({"view": "nope"})).is_err());
        let minting = minting(&request(&serde_json::json!({})), &CHAT).unwrap();
        assert_eq!(render(&minting, &page(None)).unwrap_err(), NO_WEB_ENTRY);
        let mut other = page(Some("https://tilo.so/"));
        if let Some(locator) = other.locator.as_mut() {
            locator.provider = "record:so.tilo.notes:".into();
        }
        assert!(render(&minting, &other).is_err());
    }

    #[test]
    fn a_link_to_the_root_names_the_record() {
        let mut root = request(&serde_json::json!({}));
        root.resource = "chat".into();
        let minting = minting(&root, &CHAT).unwrap();
        assert_eq!(
            render(&minting, &page(Some("https://tilo.so/"))).unwrap(),
            format!("https://tilo.so/app/#/@quinten.tilo.cloud/w/{ROOM}/so.tilo.chat/-")
        );
    }
}
