//! Showing the exact resource and position a link names, once its app is
//! in front.
//!
//! The opener focuses or launches the app; this then runs the resource's
//! declared view action through the session's action door, as the local
//! person, on the provider the app publishes for the linked record. A span
//! runs the view's range action instead. Every target the door does not
//! offer fails with [`UNAVAILABLE`], whether it is missing or refused, so a
//! link never tells anyone what exists.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tilo_actions::remote::{
    ActionHost, Caller, CataloguePage, CatalogueQuery, InvokeArgument, InvokeRequest,
    MAX_CATALOGUE_PAGE, ReceiptStatus, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, ActionDescriptor};
use tilo_reference::SpanUnit;
use tilo_reference::link::ResourceAddress;

/// The one answer for a target that is missing or not this person's.
pub const UNAVAILABLE: &str =
    "This link points at something that is not there, or that you cannot open";

/// How long a just-launched app has to publish the linked resource's view.
pub const PUBLISH_TIMEOUT: Duration = Duration::from_secs(15);

/// How often the catalogue is asked again while the app starts. Publication
/// has no change feed on the action door, and a launch takes seconds, so a
/// short bounded poll costs a handful of in-process queries.
const POLL: Duration = Duration::from_millis(150);

/// The session's action door, as the opener sees it.
pub trait LinkDoor: Send + Sync {
    /// The host and the local person it admits in `room`.
    ///
    /// # Errors
    /// The session has no action broker yet, or this device's person is not
    /// admitted in `room`.
    fn session(&self, room: &str) -> Result<(Arc<dyn ActionHost + Send + Sync>, Caller), String>;
}

/// What a link asks to be shown, once its app is in front.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reveal {
    pub room: String,
    pub app_id: String,
    /// The linked record; `None` for the app's primary record.
    pub record: Option<String>,
    pub resource: ResourceAddress,
    /// The named view; `None` asks for the primary one.
    pub view: Option<String>,
}

/// The view action (and arguments) that shows `resource` in view `view`
/// (`None`: the primary view, else the first), from the provider's current
/// descriptors.
///
/// # Errors
/// [`UNAVAILABLE`] when no such view is offered; a span on a view that
/// cannot show ranges is refused on its own terms.
pub fn view_invocation(
    actions: &[ActionDescriptor],
    resource: &ResourceAddress,
    view: Option<&str>,
) -> Result<(String, InvokeArgument), String> {
    // A view's open action carries the view's descriptor; nothing else does.
    let views: Vec<_> = actions
        .iter()
        .filter(|action| action.address.resource == resource.id)
        .filter_map(|action| {
            let descriptor = action.view.as_ref()?;
            (descriptor.open_action == action.address.action).then_some((action, descriptor))
        })
        .collect();
    let chosen = match view {
        Some(id) => views.iter().find(|(_, descriptor)| descriptor.id == id),
        None => views
            .iter()
            .find(|(_, descriptor)| descriptor.primary)
            .or_else(|| views.first()),
    };
    let Some((action, descriptor)) = chosen else {
        return Err(UNAVAILABLE.into());
    };
    let Some(span) = resource.span else {
        return Ok((action.address.action.clone(), InvokeArgument::None));
    };
    let Some(range) = &descriptor.range else {
        return Err("This view cannot show a range of its text".into());
    };
    let unit = match span.unit {
        SpanUnit::Chars => "Chars",
        SpanUnit::Bytes => "Bytes",
    };
    let mut args = serde_json::Map::new();
    args.insert(range.start_param.clone(), span.start.into());
    args.insert(range.end_param.clone(), span.end.into());
    args.insert(range.unit_param.clone(), unit.into());
    Ok((range.action.clone(), InvokeArgument::Json(args)))
}

/// Run `reveal`'s view action through `door`, waiting up to `timeout` for a
/// just-launched app to publish it. Blocks the calling thread; the opener
/// runs it off the shell's thread.
///
/// # Errors
/// The door is unavailable, or [`UNAVAILABLE`] for a target it does not
/// offer by the deadline or refuses.
pub fn reveal(door: &dyn LinkDoor, reveal: &Reveal, timeout: Duration) -> Result<(), String> {
    let now = Instant::now();
    let deadline = now.checked_add(timeout).unwrap_or(now);
    let (host, caller) = door.session(&reveal.room)?;
    let (provider, actions) = loop {
        if let Some(found) = published(host.as_ref(), &caller, reveal, deadline) {
            break found;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(UNAVAILABLE.into());
        }
        std::thread::sleep(POLL.min(left));
    };
    let (action, argument) = view_invocation(&actions, &reveal.resource, reveal.view.as_deref())?;
    let request = InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider,
            address: ActionAddress::new(reveal.resource.id.clone(), action),
        },
        argument,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    };
    let left = deadline.saturating_duration_since(Instant::now());
    match host.invoke(&caller, request).wait_timeout(left.max(POLL)) {
        Ok(Some(Ok(receipt)))
            if matches!(
                receipt.status,
                ReceiptStatus::Accepted | ReceiptStatus::Completed { .. }
            ) =>
        {
            Ok(())
        }
        // The door's own reason could tell "missing" from "refused".
        _ => Err(UNAVAILABLE.into()),
    }
}

/// The live provider publishing the linked record's app, with the view
/// descriptors it offers on the resource, once it offers at least one.
fn published(
    host: &(dyn ActionHost + Send + Sync),
    caller: &Caller,
    reveal: &Reveal,
    deadline: Instant,
) -> Option<(String, Vec<ActionDescriptor>)> {
    let providers = ask(
        host,
        caller,
        CatalogueQuery {
            max_results: 0,
            ..CatalogueQuery::default()
        },
        deadline,
    )?
    .providers;
    let record = reveal.record.as_deref().unwrap_or("");
    // Only a running connection (a numeric token) is the app now on screen;
    // launchable and application tokens would start another copy.
    let mut live: Vec<_> = providers
        .into_iter()
        .filter(|provider| {
            provider.app_id == reveal.app_id
                && provider.room == reveal.room
                && provider.provider.parse::<u64>().is_ok()
        })
        .collect();
    live.sort_by_key(|provider| provider.record_id != record);
    for provider in live {
        let page = ask(
            host,
            caller,
            CatalogueQuery {
                provider: Some(provider.provider.clone()),
                needle: reveal.resource.id.clone(),
                max_results: MAX_CATALOGUE_PAGE,
                ..CatalogueQuery::default()
            },
            deadline,
        )?;
        let actions: Vec<_> = page
            .rows
            .into_iter()
            .filter_map(|row| row.action)
            .filter(|action| action.address.resource == reveal.resource.id && action.view.is_some())
            .collect();
        if !actions.is_empty() {
            return Some((provider.provider, actions));
        }
    }
    None
}

fn ask(
    host: &(dyn ActionHost + Send + Sync),
    caller: &Caller,
    query: CatalogueQuery,
    deadline: Instant,
) -> Option<CataloguePage> {
    let left = deadline.saturating_duration_since(Instant::now());
    host.query(caller, query)
        .wait_timeout(left.max(POLL))
        .ok()
        .flatten()?
        .ok()
}
