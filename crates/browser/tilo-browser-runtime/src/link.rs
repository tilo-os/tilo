//! The page's link parser and formatter: the Rust grammar in
//! `tilo_reference::link`, exported so the page never parses a route itself
//! and cannot drift from native, relay and agent code.

use tilo_reference::link::{
    Link, LinkSecrets, LinkTarget, Mode, ModeJson, RouteJson, WebEntries, WebEntry,
};
use wasm_bindgen::prelude::*;

/// The page's entry as `entry.json` and the entry pages describe it.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryJson {
    base: String,
}

fn refuse(error: impl std::fmt::Display) -> JsValue {
    js_sys::Error::new(&error.to_string()).into()
}

/// A refusal the page can branch on: `code` is the corpus' stable code.
fn link_error(error: &tilo_reference::link::LinkError) -> JsValue {
    let value: JsValue = js_sys::Error::new(&error.to_string()).into();
    let _ = js_sys::Reflect::set(&value, &"code".into(), &error.code().into());
    value
}

fn entry(json: &str) -> Result<WebEntry, JsValue> {
    let entry: EntryJson = serde_json::from_str(json).map_err(refuse)?;
    WebEntry::new(&entry.base).map_err(|e| link_error(&e))
}

fn route(json: &str) -> Result<Link, JsValue> {
    let route: RouteJson = serde_json::from_str(json).map_err(refuse)?;
    Ok(Link {
        mode: match route.mode {
            ModeJson::Desktop => Mode::Desktop,
            ModeJson::App => Mode::App,
        },
        target: LinkTarget::try_from(route.target).map_err(|e| link_error(&e))?,
        // Secrets never cross into the page's route form; the scrubber holds them.
        secrets: LinkSecrets::default(),
    })
}

/// Parse `url` under the page's entry (`entryJson`, `{base}`) into
/// `{mode, target}` JSON. A link carrying secrets is refused here like any
/// other: the inline scrubber removed them before any module ran.
///
/// A link whose workspace reads but whose part inside it is damaged (a
/// document id that is no id) names that workspace's desktop, with
/// `damaged`: what of the link was not used, in words for the person
/// ([`Link::parse_or_workspace`]).
#[wasm_bindgen(js_name = parseLink)]
pub fn parse_link(url: &str, entry_json: &str) -> Result<String, JsValue> {
    let entries = WebEntries::new(vec![entry(entry_json)?]);
    let (link, damaged) = Link::parse_or_workspace(url, &entries).map_err(|e| link_error(&e))?;
    if !link.secrets.is_empty() {
        return Err(refuse("This address still carries a secret"));
    }
    route_json(&link, damaged.as_ref()).map_err(refuse)
}

/// `link` as the page's `{mode, target}` JSON, saying what of the link was
/// `damaged` when only its workspace was read.
fn route_json(
    link: &Link,
    damaged: Option<&tilo_reference::link::LinkError>,
) -> Result<String, serde_json::Error> {
    let mut json = serde_json::to_value(RouteJson::from(link))?;
    if let (Some(damaged), Some(fields)) = (damaged, json.as_object_mut()) {
        fields.insert("damaged".into(), damaged.to_string().into());
    }
    serde_json::to_string(&json)
}

/// Write `{mode, target}` JSON as a URL under the entry.
///
/// # Errors
///
/// A target that cannot be written in exactly one way.
#[wasm_bindgen(js_name = formatLink)]
pub fn format_link(route_json: &str, entry_json: &str) -> Result<String, JsValue> {
    route(route_json)?
        .to_url(&entry(entry_json)?)
        .map_err(|e| link_error(&e))
}

/// The link `route_json` names, carrying `invitation` (an invitation's
/// text) as its `invite` secret: a link that lets anyone who has it in, to
/// exactly the place it names. The page hands it to the clipboard and keeps
/// no copy; the scrubber takes the secret out of the address bar of the
/// page it opens.
///
/// # Errors
///
/// As [`format_link`], or an invitation that is not a link secret.
#[wasm_bindgen(js_name = formatInviteLink)]
pub fn format_invite_link(
    route_json: &str,
    entry_json: &str,
    invitation: &str,
) -> Result<String, JsValue> {
    let mut link = route(route_json)?;
    link.secrets.invite =
        Some(tilo_reference::link::Secret::new(invitation).map_err(|e| link_error(&e))?);
    link.to_url(&entry(entry_json)?).map_err(|e| link_error(&e))
}

/// `tilo://open#<route>` for an installed Tilo, with the scope explicit.
///
/// # Errors
///
/// As [`format_link`].
#[wasm_bindgen(js_name = nativeLink)]
pub fn native_link(route_json: &str) -> Result<String, JsValue> {
    route(route_json)?.to_native().map_err(|e| link_error(&e))
}
