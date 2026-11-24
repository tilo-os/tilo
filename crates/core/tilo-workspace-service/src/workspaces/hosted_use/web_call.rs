//! A web call that carries a key of its person's vault: the one use whose
//! secret leaves this server inside a request an agent wrote.
//!
//! Most services have no connection of their own and take a key with each
//! request. An agent names the request (its address, its method, what it
//! sends) and the vault item that is the key, by reference. This server adds
//! the key on the way out, where the item says its service takes it
//! ([`KeyIn`]), and only to the service the item belongs to: the origin of
//! the address is the place the vault is asked for, so a key kept for one
//! service is refused for a call to any other.
//!
//! The agent wrote everything else of the request, so the request is held to
//! what cannot carry the key somewhere else. The address is https (plain
//! http only to a literal address inside this server's own network, where
//! its operator lets it call there), names nobody to sign in as, and no
//! redirect is followed. A name is looked up once, by the client that makes
//! the connection: every address the lookup answers is checked, and the
//! connection goes to exactly what was checked, so a name that answers an
//! address inside this server's own network, alone or beside a public one,
//! is refused before anything is dialled. The headers that frame a request, say who it is
//! for or sign it in are not the agent's to set, and neither is the header
//! or the parameter the key rides in.
//!
//! What a call sends is the agent's too: text, the parts of a multipart body
//! (text or JSON inline, or content the workspace keeps by its BLAKE3
//! address with its filename and type), or one content as the whole body.
//! The text and JSON are bounded; content is not, beyond what the workspace's
//! store holds: it is read from that store into the request by the vault, so
//! it never passes through the app's frames, and only content this workspace
//! keeps for the app that asks is sent: anything else is refused with one
//! reason, before the person is asked. How many bytes were sent is written
//! down with the use, never what they were.
//!
//! What the service answers goes back to the agent with the key struck out
//! wherever it comes back as it was sent: as it is, percent-encoded, in
//! base64, in hexadecimal, or escaped for JSON or a page. A key too short
//! for that to mean anything is not sent. No refusal names the address as it
//! was sent or anything the request carried, and nothing here writes a log
//! line about a request.
//!
//! What this does not hold: a service that hands out another credential in
//! its answer (a new key, a session) hands it to the agent, and a service
//! that returns its own key in some other form is not stopped. So nobody's
//! "Always" stands for a call. Each one is shown to its person first, with
//! its method, its address and what it sends, and their approve admits that
//! very call again, which is then sent once.

use std::time::Duration;

use accounts::vault_items::{self, ItemId, KeyIn, Place, UsedFor, VaultItems};
use serde_json::{Map, Value, json};
use tilo_net::{HeaderMap, HeaderName, HeaderValue, Method, Url};

use tilo_actions::remote::person_vault::ContentRef;
pub(super) use tilo_actions::remote::person_vault::WebCall;

use super::Workspaces;
// The guard's own names, from the one place this crate takes them.
use crate::connections::wire::{
    Destinations, Lookup, check_destination, guarded_client, is_non_public_literal,
    is_withheld_request_header, refused_address, system_lookup,
};

/// How long a service may take to answer, whole.
const DEADLINE: Duration = Duration::from_secs(30);
/// The longest address called.
const MAX_ADDRESS_BYTES: usize = 2_048;
/// The most text and JSON one call sends.
const MAX_SENT_BYTES: usize = 256 * 1024;
/// The most of an answer handed back.
const MAX_ANSWER_BYTES: usize = 256 * 1024;
/// The most headers a call names, and the longest of them.
const MAX_HEADERS: usize = 32;
const MAX_HEADER_BYTES: usize = 4 * 1024;
/// The most parts a call's body holds, and the longest a part's field name.
const MAX_PARTS: usize = 64;
const MAX_PART_NAME_BYTES: usize = 200;
/// The most headers of an answer handed back, and the most of each.
const MAX_ANSWER_HEADERS: usize = 64;
const MAX_ANSWER_HEADER_BYTES: usize = 2 * 1024;
/// What stands in an answer where the key stood.
const WITHHELD: &str = "[withheld]";

const NOT_AN_ADDRESS: &str = "A web call names the web address it calls";
const UNREACHED: &str = "The service could not be reached";
const NO_ANSWER_IN_TIME: &str = "The service did not answer in time";
const CANNOT_RIDE: &str = "That key cannot be sent the way its item says";
const INSIDE: &str = "That address is inside this server's own network, where no key is sent";
const MANY_BODIES: &str =
    "A web call sends its body as text, as parts, or as workspace content, not more than one";
const NOT_A_PART: &str = "A web call's body is parts, each a field name and its text, its JSON, \
     or the content it carries";
const TOO_MUCH: &str = "That is more text or JSON than one web call sends";
const NOT_KEPT: &str = "A web call sends only content its workspace keeps for the app that asks";
const NOT_AN_ADDRESS_OF_CONTENT: &str =
    "Content is named by the 64 lowercase hex characters of its BLAKE3 address";

/// Whether `name`, in lower case, is a header that is not the agent's to set:
/// the shared withheld set ([`is_withheld_request_header`]), credentials, and
/// headers that turn the request into another request at the service.
fn senders_own(name: &str) -> bool {
    is_withheld_request_header(name)
        || matches!(
            name,
            "authorization"
                | "cookie"
                | "x-http-method"
                | "x-http-method-override"
                | "x-method-override"
                | "x-original-url"
                | "x-rewrite-url"
        )
}

/// Whether `name`, in lower case, is a header of an answer that is not handed
/// back: a session the service opened for the key is a credential of its own.
fn not_handed_back(name: &str) -> bool {
    matches!(
        name,
        "authorization"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "set-cookie"
            | "set-cookie2"
    )
}

impl Workspaces {
    /// Which addresses this server sends a key to: those on the public
    /// internet, and its own network too only where its operator lets it
    /// call there, the one leave a person's model provider is called by
    /// ([`Self::call_private_model_addresses`]).
    pub(super) fn call_reach(&self) -> Destinations {
        if self.inner.models.private_addresses() {
            Destinations::Any
        } else {
            Destinations::Public
        }
    }
}

/// The address `call` names, when a key may be sent there by a server that
/// reaches `reach`.
fn address(call: &WebCall, reach: Destinations) -> Result<Url, String> {
    let text = call.url.trim();
    if text.is_empty() || text.len() > MAX_ADDRESS_BYTES {
        return Err(NOT_AN_ADDRESS.to_owned());
    }
    let mut url = Url::parse(text).map_err(|_| NOT_AN_ADDRESS.to_owned())?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(
            "The address of a web call names nobody to sign in as: its key is its sign-in"
                .to_owned(),
        );
    }
    let plain_inside =
        url.scheme() == "http" && reach == Destinations::Any && is_non_public_literal(&url);
    if url.scheme() != "https" && !plain_inside {
        return Err("A key is sent only to an https address".to_owned());
    }
    check_destination(&url, reach)
        .map_err(|_| "That address is not one a key is sent to".to_owned())?;
    url.set_fragment(None);
    Ok(url)
}

/// The method `call` names: a `GET` when it names none.
fn method(call: &WebCall) -> Result<Method, String> {
    match call.method.trim().to_ascii_uppercase().as_str() {
        "" | "GET" => Ok(Method::GET),
        "HEAD" => Ok(Method::HEAD),
        "POST" => Ok(Method::POST),
        "PUT" => Ok(Method::PUT),
        "PATCH" => Ok(Method::PATCH),
        "DELETE" => Ok(Method::DELETE),
        _ => Err("A web call is a GET, HEAD, POST, PUT, PATCH or DELETE".to_owned()),
    }
}

/// The headers `call` names besides its key, when each is the agent's to
/// set where the key rides as `key_in` says.
fn headers(call: &WebCall, key_in: &KeyIn) -> Result<Vec<(HeaderName, HeaderValue)>, String> {
    if call.headers.len() > MAX_HEADERS {
        return Err("A web call names a few dozen headers at most".to_owned());
    }
    let keys = key_in.header();
    let mut sent = Vec::with_capacity(call.headers.len());
    for (name, value) in &call.headers {
        let lower = name.trim().to_ascii_lowercase();
        if senders_own(&lower) || keys.as_deref() == Some(lower.as_str()) {
            return Err(format!(
                "The header {lower} is not yours to set: the server sets what frames a request \
                 and what signs it in"
            ));
        }
        let not_a_header = || "A header is a name and one line of text".to_owned();
        if value.len() > MAX_HEADER_BYTES {
            return Err(not_a_header());
        }
        let name = HeaderName::from_bytes(lower.as_bytes()).map_err(|_| not_a_header())?;
        let value = HeaderValue::from_str(value).map_err(|_| not_a_header())?;
        sent.push((name, value));
    }
    Ok(sent)
}

/// Whether the address leaves the key's own parameter to the key.
fn rides_alone(url: &Url, key_in: &KeyIn) -> Result<(), String> {
    match key_in {
        KeyIn::Query { name } if url.query_pairs().any(|(named, _)| named == *name) => Err(
            format!("The parameter {name} is not yours to set: the key rides in it"),
        ),
        _ => Ok(()),
    }
}

fn item_of(call: &WebCall) -> Result<ItemId, String> {
    ItemId::parse(call.key.id()).ok_or_else(|| vault_items::ItemError::NotFound.to_string())
}

/// What `call` wants of the vault: its key, which must be one, and the one
/// place the key would be sent to, the origin of the address.
pub(super) fn wanted(
    call: &WebCall,
) -> Result<(ItemId, vault_items::ItemKind, Vec<Place>), String> {
    let url = Url::parse(call.url.trim()).map_err(|_| NOT_AN_ADDRESS.to_owned())?;
    let service =
        Place::parse(&url.origin().ascii_serialization()).map_err(|_| NOT_AN_ADDRESS.to_owned())?;
    Ok((item_of(call)?, vault_items::ItemKind::Key, vec![service]))
}

/// What `call` would be refused for when it is sent by a server that
/// reaches `reach`, with the vault's `items`: said before anyone is
/// asked about it. Nothing is handed out and nothing is written down.
pub(super) fn sendable(
    call: &WebCall,
    reach: Destinations,
    items: &VaultItems,
) -> Result<(), String> {
    let url = address(call, reach)?;
    method(call)?;
    check_body(call)?;
    let item = item_of(call)?;
    let key_in = items.key_in(&item).map_err(|error| error.to_string())?;
    headers(call, &key_in)?;
    rides_alone(&url, &key_in)
}

/// [`sendable`] with the content `call` names read from the store `workspace` keeps it in: what
/// this workspace does not keep for the app that asks, or for the app the reference names, is
/// refused here, before anyone is asked about the call. `caller_app` is the app that asks.
pub(super) fn sendable_content(
    call: &WebCall,
    workspaces: &super::Workspaces,
    workspace: &str,
    caller_app: Option<&str>,
) -> Result<(), String> {
    let content = |app: &str, hash: &str| workspaces.kept_record_bytes(workspace, app, hash);
    for reference in call.body_content.iter().chain(
        call.body_parts
            .iter()
            .filter_map(|part| part.content.as_ref()),
    ) {
        content_bytes(reference, caller_app, &content)?;
    }
    Ok(())
}

/// Whether `call`'s body reads: one of text, parts or content, each part one
/// of text, JSON or content, its field named, and no more text or JSON than a
/// call sends. Content is not counted here: it is bounded only by what the
/// workspace's store keeps, and what a part names is read from that store
/// separately ([`body_of`]): nothing here opens it.
fn check_body(call: &WebCall) -> Result<(), String> {
    let named = usize::from(!call.body.is_empty())
        .saturating_add(usize::from(!call.body_parts.is_empty()))
        .saturating_add(usize::from(call.body_content.is_some()));
    if named > 1 {
        return Err(MANY_BODIES.to_owned());
    }
    if sets_its_own_type(call) {
        return Err(NOT_A_PART.to_owned());
    }
    if call.body.len() > MAX_SENT_BYTES {
        return Err(TOO_MUCH.to_owned());
    }
    if call.body_parts.len() > MAX_PARTS {
        return Err(NOT_A_PART.to_owned());
    }
    let mut inline = call.body.len();
    for part in &call.body_parts {
        let name = part.name.trim();
        if name.is_empty() || name.len() > MAX_PART_NAME_BYTES || name.chars().any(char::is_control)
        {
            return Err(NOT_A_PART.to_owned());
        }
        let held = usize::from(part.text.is_some())
            .saturating_add(usize::from(part.json.is_some()))
            .saturating_add(usize::from(part.content.is_some()));
        if held != 1 {
            return Err(NOT_A_PART.to_owned());
        }
        if let Some(text) = &part.text {
            inline = inline.saturating_add(text.len());
        }
        if let Some(json) = &part.json {
            inline = inline.saturating_add(
                serde_json::to_vec(json)
                    .map_err(|error| error.to_string())?
                    .len(),
            );
        }
        if let Some(content) = &part.content {
            check_content(content)?;
            check_media_type(&content.content_type)?;
        }
    }
    if let Some(content) = &call.body_content {
        check_content(content)?;
        check_media_type(&content.content_type)?;
    }
    if inline > MAX_SENT_BYTES {
        return Err(TOO_MUCH.to_owned());
    }
    Ok(())
}

/// Whether `content` names bytes by a lowercase BLAKE3 address, in an app's
/// records that is named on one line.
fn check_content(content: &ContentRef) -> Result<(), String> {
    let hash = content.hash.trim();
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(NOT_AN_ADDRESS_OF_CONTENT.to_owned());
    }
    if !content.app.trim().is_empty() && !names_a_scope(content.app.trim()) {
        return Err(NOT_A_PART.to_owned());
    }
    if content.filename.len() > MAX_PART_NAME_BYTES
        || content.filename.chars().any(char::is_control)
    {
        return Err(NOT_A_PART.to_owned());
    }
    Ok(())
}

/// Whether `name` is what a content store calls a scope: a plain name, never a path.
fn names_a_scope(name: &str) -> bool {
    name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        && !name.bytes().all(|byte| byte == b'.')
}

/// Whether `content_type` could ride in a part's or the request's headers.
fn check_media_type(content_type: &str) -> Result<(), String> {
    if content_type.len() > MAX_HEADER_BYTES || content_type.chars().any(char::is_control) {
        return Err(NOT_A_PART.to_owned());
    }
    Ok(())
}

/// What a call sends, ready to be put in the request.
struct Sending {
    /// The request's `Content-Type`, when it names one.
    content_type: Option<String>,
    bytes: Vec<u8>,
}

/// One part of a multipart body, its bytes in hand.
struct SentPart {
    name: String,
    filename: Option<String>,
    content_type: String,
    bytes: Vec<u8>,
}

/// What `call` sends: its text, the parts it names with the content they
/// carry read from the workspace's store, or one content as the whole body.
/// `caller` is the app that asks, which content naming no app of its own is
/// read as, and `content` answers (`app`, `hash`) with the bytes the
/// workspace keeps for that app, or none.
fn body_of(
    call: &WebCall,
    caller: Option<&str>,
    content: &dyn Fn(&str, &str) -> Option<Vec<u8>>,
) -> Result<Option<Sending>, String> {
    check_body(call)?;
    if !call.body.is_empty() {
        return Ok(Some(Sending {
            content_type: None,
            bytes: call.body.clone().into_bytes(),
        }));
    }
    if let Some(reference) = &call.body_content {
        let bytes = content_bytes(reference, caller, content)?;
        return Ok(Some(Sending {
            content_type: Some(media_type(reference, "application/octet-stream")),
            bytes,
        }));
    }
    if call.body_parts.is_empty() {
        return Ok(None);
    }
    let mut parts = Vec::with_capacity(call.body_parts.len());
    for part in &call.body_parts {
        let (bytes, content_type) = match (&part.text, &part.json, &part.content) {
            (Some(text), _, _) => (
                text.clone().into_bytes(),
                "text/plain; charset=utf-8".to_owned(),
            ),
            (_, Some(json), _) => (
                serde_json::to_vec(json).map_err(|error| error.to_string())?,
                "application/json".to_owned(),
            ),
            (_, _, Some(reference)) => (
                content_bytes(reference, caller, content)?,
                media_type(reference, "application/octet-stream"),
            ),
            _ => return Err(NOT_A_PART.to_owned()),
        };
        let filename = part
            .content
            .as_ref()
            .map(|content| content.filename.trim())
            .filter(|filename| !filename.is_empty())
            .map(str::to_owned);
        parts.push(SentPart {
            name: part.name.trim().to_owned(),
            filename,
            content_type,
            bytes,
        });
    }
    multipart(parts)
}

/// The bytes `reference` names, from the workspace's store: the one place
/// content a call carries is read. Refused with one reason, which names
/// neither the address nor the app it was looked for under.
fn content_bytes(
    reference: &ContentRef,
    caller: Option<&str>,
    content: &dyn Fn(&str, &str) -> Option<Vec<u8>>,
) -> Result<Vec<u8>, String> {
    let hash = reference.hash.trim();
    let app = match reference.app.trim() {
        "" => caller.unwrap_or_default(),
        named => named,
    };
    let bytes = content(app, hash).ok_or_else(|| NOT_KEPT.to_owned())?;
    if !tilo_content::verify(hash, &bytes) {
        return Err(NOT_KEPT.to_owned());
    }
    Ok(bytes)
}

/// The media type a content part is sent as: its own, or `fallback`.
fn media_type(reference: &ContentRef, fallback: &str) -> String {
    match reference.content_type.trim() {
        "" => fallback.to_owned(),
        named => named.to_owned(),
    }
}

/// `parts` framed as one multipart body, its boundary one no part's bytes
/// hold.
fn multipart(parts: Vec<SentPart>) -> Result<Option<Sending>, String> {
    let mut boundary = String::from("tilo-web-call-boundary-7f3c9a");
    while parts.iter().any(|part| {
        part.bytes
            .windows(boundary.len())
            .any(|at| at == boundary.as_bytes())
    }) {
        boundary.push('x');
    }
    let mut bytes = Vec::new();
    for part in &parts {
        let name = part.name.replace(['"', '\r', '\n'], "_");
        let filename = part
            .filename
            .as_deref()
            .map_or_else(String::new, |filename| {
                format!(
                    "; filename=\"{}\"",
                    filename.replace(['"', '\r', '\n'], "_")
                )
            });
        bytes.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"{filename}\r\n\
                 Content-Type: {}\r\n\r\n",
                part.content_type
            )
            .as_bytes(),
        );
        bytes.extend_from_slice(&part.bytes);
        bytes.extend_from_slice(b"\r\n");
    }
    bytes.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    Ok(Some(Sending {
        content_type: Some(format!("multipart/form-data; boundary={boundary}")),
        bytes,
    }))
}

/// A header that carries the key: its value prints as withheld.
fn key_header(name: &str, text: &str) -> Result<(HeaderName, HeaderValue), String> {
    let name = HeaderName::from_bytes(name.to_ascii_lowercase().as_bytes())
        .map_err(|_| CANNOT_RIDE.to_owned())?;
    let mut value = HeaderValue::from_str(text).map_err(|_| CANNOT_RIDE.to_owned())?;
    value.set_sensitive(true);
    Ok((name, value))
}

/// What a call does, as its key's use is written down: the method, the
/// path and how many bytes it sent. Never the address's query, where a key
/// may ride and where whoever wrote the request may have put anything, and
/// never what is sent.
fn in_a_line(method: &Method, url: &Url, sent: usize) -> String {
    if sent == 0 {
        format!("{} {}", method.as_str(), url.path())
    } else {
        format!("{} {} ({sent} bytes)", method.as_str(), url.path())
    }
}

/// Whether `call` names the content type its body is framed with, which the
/// server sets.
fn sets_its_own_type(call: &WebCall) -> bool {
    let body_sets_one = call.body_content.is_some() || !call.body_parts.is_empty();
    body_sets_one
        && call
            .headers
            .keys()
            .any(|name| name.trim().eq_ignore_ascii_case("content-type"))
}

/// Make `call` for `by` with the key `workspace`'s vault keeps, from a server that reaches what
/// `workspaces` reaches, its name looked up as every server's own request looks one up
/// ([`deliver`]). `purpose` is what the key's use is written down with: the use's word and how
/// its person allowed it. The use is written to the vault before the request leaves.
pub(super) async fn send(
    workspaces: &super::Workspaces,
    workspace: &str,
    call: WebCall,
    (by, asker): (&str, &tilo_action_policy::Acting),
    caller_app: Option<&str>,
    purpose: UsedFor<'_>,
) -> Result<Value, String> {
    let reach = workspaces.call_reach();
    let opened = workspaces.open_vault(workspace).await?;
    let mut items = opened.items();
    let content = |app: &str, hash: &str| workspaces.kept_record_bytes(workspace, app, hash);
    let ready = ready(reach, &mut items, call, by, caller_app, purpose, &content)?;
    workspaces
        .write_vault(&opened, items.take_changes(), Some(asker))
        .await?;
    deliver(reach, system_lookup(), ready).await
}

/// [`send`] with the vault's `items` in hand, looking a name up with `lookup`
/// and reading the content a call carries with `content`.
#[cfg(test)]
#[allow(
    clippy::too_many_arguments,
    reason = "a test names each part of the call it makes, in one chain"
)]
async fn send_through(
    reach: Destinations,
    lookup: Lookup,
    items: &mut VaultItems,
    call: WebCall,
    by: &str,
    caller_app: Option<&str>,
    purpose: UsedFor<'_>,
    content: &dyn Fn(&str, &str) -> Option<Vec<u8>>,
) -> Result<Value, String> {
    let ready = ready(reach, items, call, by, caller_app, purpose, content)?;
    deliver(reach, lookup, ready).await
}

/// A call with its key in place, nothing of it sent yet.
struct Ready {
    url: Url,
    method: Method,
    sent: Vec<(HeaderName, HeaderValue)>,
    struck: Struck,
    body: Option<Sending>,
}

/// `call` for `by` with the key the vault's `items` keep, from a server that reaches `reach`:
/// the one place the reference becomes its secret. What the call carries is read from the
/// workspace's store first (`content`), so content it does not keep is refused before the key is
/// anywhere. The use is written down in `items` with `purpose` and with what this call does
/// ([`in_a_line`]), whatever `purpose` says of that.
fn ready(
    reach: Destinations,
    items: &mut VaultItems,
    call: WebCall,
    by: &str,
    caller_app: Option<&str>,
    purpose: UsedFor<'_>,
    content: &dyn Fn(&str, &str) -> Option<Vec<u8>>,
) -> Result<Ready, String> {
    let mut url = address(&call, reach)?;
    let method = method(&call)?;
    let body = body_of(&call, caller_app, content)?;
    let (item, kind, places) = wanted(&call)?;
    // The key leaves the vault last, once nothing else refuses the call,
    // and its use is written down before it is handed out.
    let key_in = items.key_in(&item).map_err(|error| error.to_string())?;
    let mut sent = headers(&call, &key_in)?;
    rides_alone(&url, &key_in)?;
    // Said before the key is anywhere in the request.
    let what = in_a_line(
        &method,
        &url,
        body.as_ref().map_or(0, |body| body.bytes.len()),
    );
    let purpose = UsedFor {
        what: &what,
        ..purpose
    };
    let key = items
        .use_for(&item, kind, &places, by, purpose)
        .map_err(|error| error.to_string())?;
    let struck = keyed(&mut url, &mut sent, &key_in, &key)?;
    Ok(Ready {
        url,
        method,
        sent,
        struck,
        body,
    })
}

/// Send what [`ready`] made from a server that reaches `reach` and looks a name up with
/// `lookup`. The client asks `lookup` once, checks every address it answers and connects to
/// exactly those. Answers the status, the headers and the body, the key struck out of all of it.
async fn deliver(reach: Destinations, lookup: Lookup, ready: Ready) -> Result<Value, String> {
    let Ready {
        url,
        method,
        sent,
        struck,
        body: sending,
    } = ready;
    let client = guarded_client(reach, lookup).map_err(|_| UNREACHED.to_owned())?;
    let mut request = client.request(method, url).timeout(DEADLINE);
    for (name, value) in sent {
        request = request.header(name, value);
    }
    if let Some(sending) = sending {
        if let Some(content_type) = sending.content_type {
            request = request.header("content-type", content_type);
        }
        request = request.body(sending.bytes);
    }
    // The address may carry the key, and an error of the client's names the
    // address: no refusal here repeats what the client says.
    let mut response = request.send().await.map_err(|error| {
        if refused_address(&error) {
            INSIDE.to_owned()
        } else if error.is_timeout() {
            NO_ANSWER_IN_TIME.to_owned()
        } else {
            UNREACHED.to_owned()
        }
    })?;
    let status = response.status().as_u16();
    let answered = handed_back(response.headers(), &struck);
    // Read a little past what is handed back, so a key that stands across
    // the cut is struck whole and not handed back by its beginning.
    let window = MAX_ANSWER_BYTES.saturating_add(struck.longest());
    let mut body: Vec<u8> = Vec::new();
    let mut more = false;
    loop {
        let chunk = response.chunk().await.map_err(|error| {
            if error.is_timeout() {
                NO_ANSWER_IN_TIME.to_owned()
            } else {
                "The service's answer was cut short".to_owned()
            }
        })?;
        let Some(chunk) = chunk else {
            break;
        };
        let left = window.saturating_sub(body.len());
        if chunk.len() > left {
            body.extend(chunk.iter().take(left));
            more = true;
            break;
        }
        body.extend_from_slice(&chunk);
    }
    let truncated = more || body.len() > MAX_ANSWER_BYTES;
    Ok(json!({
        "status": status,
        "headers": answered,
        "body": struck.out_of(&body, MAX_ANSWER_BYTES),
        "truncated": truncated,
    }))
}

/// Put `key` where `key_in` says it rides: into `sent`, or into the address.
/// Answers the forms it is struck out of the answer in.
fn keyed(
    url: &mut Url,
    sent: &mut Vec<(HeaderName, HeaderValue)>,
    key_in: &KeyIn,
    key: &vault_items::Released,
) -> Result<Struck, String> {
    let secret = key.secret.as_str();
    match key_in {
        KeyIn::Bearer => sent.push(key_header("authorization", &format!("Bearer {secret}"))?),
        KeyIn::Basic => {
            // The item's name and the key; the key as the name, with no
            // password, where the item has no name.
            let pair = if key.username.is_empty() {
                format!("{secret}:")
            } else {
                format!("{}:{secret}", key.username)
            };
            let sealed = String::from_utf8(base64(pair.as_bytes(), STANDARD, true))
                .map_err(|_| CANNOT_RIDE.to_owned())?;
            sent.push(key_header("authorization", &format!("Basic {sealed}"))?);
            return Ok(Struck::of(&[secret, pair.as_str()]));
        }
        KeyIn::Header { name, scheme } if scheme.is_empty() => {
            sent.push(key_header(name, secret)?);
        }
        KeyIn::Header { name, scheme } => {
            sent.push(key_header(name, &format!("{scheme} {secret}"))?);
        }
        KeyIn::Query { name } => {
            url.query_pairs_mut().append_pair(name, secret);
        }
    }
    Ok(Struck::of(&[secret]))
}

/// The headers of an answer as they are handed back: the key struck out of
/// each, and none that carries a session.
fn handed_back(headers: &HeaderMap, struck: &Struck) -> Map<String, Value> {
    let mut answered = Map::new();
    for (name, value) in headers {
        let name = name.as_str();
        if not_handed_back(name)
            || (answered.len() >= MAX_ANSWER_HEADERS && !answered.contains_key(name))
        {
            continue;
        }
        let text = struck.out_of(value.as_bytes(), MAX_ANSWER_HEADER_BYTES);
        match answered.get_mut(name) {
            Some(Value::String(kept)) => {
                kept.push_str(", ");
                kept.push_str(&text);
            }
            _ => {
                answered.insert(name.to_owned(), Value::String(text));
            }
        }
    }
    answered
}

const STANDARD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL_SAFE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// `bytes` in base64 over `alphabet`, padded when `pad`.
fn base64(bytes: &[u8], alphabet: &[u8; 64], pad: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len().div_ceil(3).saturating_mul(4));
    for chunk in bytes.chunks(3) {
        let byte = |at: usize| u32::from(chunk.get(at).copied().unwrap_or(0));
        let group = byte(0).wrapping_shl(16) | byte(1).wrapping_shl(8) | byte(2);
        let digit = |shift: u32| {
            usize::try_from(group.wrapping_shr(shift) & 63)
                .ok()
                .and_then(|at| alphabet.get(at).copied())
                .unwrap_or(b'=')
        };
        out.push(digit(18));
        out.push(digit(12));
        if chunk.len() > 1 {
            out.push(digit(6));
        } else if pad {
            out.push(b'=');
        }
        if chunk.len() > 2 {
            out.push(digit(0));
        } else if pad {
            out.push(b'=');
        }
    }
    out
}

/// `bytes` in hexadecimal, in capitals when `upper`.
fn hexadecimal(bytes: &[u8], upper: bool) -> Vec<u8> {
    let text = hex::encode(bytes);
    if upper {
        text.to_ascii_uppercase().into_bytes()
    } else {
        text.into_bytes()
    }
}

/// `bytes` percent-encoded as an address carries them, its escapes in
/// capitals when `upper`.
fn percent(bytes: &[u8], upper: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for byte in bytes {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(*byte);
        } else {
            out.push(b'%');
            out.extend(hexadecimal(&[*byte], upper));
        }
    }
    out
}

/// The forms a key is struck out of an answer in: as it was sent, and as a
/// service that repeats what it was sent may spell it.
struct Struck(Vec<Vec<u8>>);

impl Struck {
    /// The forms of each of `texts`: the key, and for a basic sign-in the
    /// name and the key as they were sent together.
    fn of(texts: &[&str]) -> Self {
        let mut forms: Vec<Vec<u8>> = Vec::new();
        for text in texts {
            let bytes = text.as_bytes();
            forms.push(bytes.to_vec());
            for upper in [true, false] {
                let escaped = percent(bytes, upper);
                // A form's field spells a blank as a plus.
                forms.push(
                    String::from_utf8_lossy(&escaped)
                        .replace("%20", "+")
                        .into_bytes(),
                );
                forms.push(escaped);
                forms.push(hexadecimal(bytes, upper));
            }
            for alphabet in [STANDARD, URL_SAFE] {
                for pad in [true, false] {
                    forms.push(base64(bytes, alphabet, pad));
                }
            }
            if let Ok(quoted) = serde_json::to_string(text) {
                forms.push(quoted.trim_matches('"').as_bytes().to_vec());
            }
            forms.push(
                text.replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;")
                    .replace('"', "&quot;")
                    .replace('\'', "&#39;")
                    .into_bytes(),
            );
        }
        forms.retain(|form| !form.is_empty());
        // The longest first, so a form that begins like a shorter one is
        // struck whole.
        forms.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        forms.dedup();
        Self(forms)
    }

    /// The length of the longest form.
    fn longest(&self) -> usize {
        self.0.first().map_or(0, Vec::len)
    }

    /// The first `most` bytes of `bytes` as text, every form that begins
    /// within them struck out whole.
    fn out_of(&self, bytes: &[u8], most: usize) -> String {
        let mut out = Vec::with_capacity(bytes.len().min(most));
        let mut at = 0;
        while at < bytes.len() && at < most {
            let rest = bytes.get(at..).unwrap_or_default();
            if let Some(form) = self.0.iter().find(|form| rest.starts_with(form)) {
                out.extend_from_slice(WITHHELD.as_bytes());
                at = at.saturating_add(form.len());
            } else {
                out.extend(rest.first());
                at = at.saturating_add(1);
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }
}

#[cfg(test)]
mod tests {

    use std::future::Future;
    use std::net::IpAddr;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// A made-up key: no service ever issued it.
    const KEY: &str = "made-up key/0123+456?789&for=tests";

    /// A workspace that keeps none of the content a call names.
    fn no_content(_app: &str, _hash: &str) -> Option<Vec<u8>> {
        None
    }

    /// A stand-in for the resolver: every name answers `answers`, and each
    /// lookup is counted.
    fn answering(answers: &[&str]) -> (Lookup, Arc<AtomicUsize>) {
        let answers: Vec<IpAddr> = answers
            .iter()
            .map(|address| address.parse().unwrap())
            .collect();
        let asked = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&asked);
        let lookup: Lookup = Arc::new(move |_name: String| {
            counted.fetch_add(1, Ordering::SeqCst);
            let answers = answers.clone();
            Box::pin(async move { Ok::<_, std::io::Error>(answers) })
                as Pin<Box<dyn Future<Output = std::io::Result<Vec<IpAddr>>> + Send>>
        });
        (lookup, asked)
    }

    /// A vault that keeps the made-up key for `service`, and the call for a number there.
    fn kept_for(service: &str) -> (VaultItems, WebCall) {
        let mut items = VaultItems::default();
        let kept = items
            .add(vault_items::NewItem::new(
                vault_items::ItemKind::Key,
                vec![Place::parse(service).unwrap()],
                String::new(),
                KEY.to_owned(),
            ))
            .unwrap();
        let call = serde_json::from_value(json!({
            "url": format!("{service}/v1/balance"),
            "key": format!("vault.item:{}", kept.id.as_str()),
        }))
        .unwrap();
        (items, call)
    }

    /// A call's parts reach the service framed as one multipart body: each
    /// part's field name, the filename and type of content, and the bytes
    /// the workspace keeps, read by the vault from the store. Content it
    /// does not keep is refused before anything is sent, and the key still
    /// rides the request as its item says.
    #[tokio::test]
    async fn a_call_sends_its_parts_with_the_content_its_workspace_keeps() {
        use test_http::{Reply, Responder};
        use tilo_actions::remote::person_vault::WebCallPart;

        let service = Responder::handle(|request| {
            Some(Reply::status(200).body(format!(
                "{}\n{}",
                request.header("content-type").unwrap_or("none"),
                request.body_text()
            )))
        });
        let picture = b"\x89PNG the picture's bytes".to_vec();
        let hash = blake3::hash(&picture).to_hex().to_string();
        let (mut items, mut call) = kept_for(&service.url());
        let item = item_of(&call).unwrap();
        call.url = format!("{}/v1/upload", service.url());
        call.method = "POST".to_owned();
        call.body_parts = vec![
            WebCallPart {
                name: "payload_json".into(),
                text: None,
                json: Some(json!({"content": "hi"})),
                content: None,
            },
            WebCallPart {
                name: "files[0]".into(),
                text: None,
                json: None,
                content: Some(ContentRef {
                    app: "so.tilo.chat".into(),
                    hash: hash.clone(),
                    filename: "a.png".into(),
                    content_type: "image/png".into(),
                }),
            },
        ];
        let keeps = |app: &str, asked: &str| {
            (app == "so.tilo.chat" && asked == hash).then(|| picture.clone())
        };
        let (lookup, _) = answering(&["127.0.0.1"]);
        let answer = send_through(
            Destinations::Any,
            lookup,
            &mut items,
            call.clone(),
            "Scout",
            None,
            UsedFor::default(),
            &keeps,
        )
        .await
        .unwrap();
        assert_eq!(answer["status"], 200);
        let sent = service.drain_requests();
        assert_eq!(sent.len(), 1, "one request");
        assert_eq!(
            sent[0].header("authorization"),
            Some(format!("Bearer {KEY}").as_str()),
            "the key still rides the request"
        );
        let content_type = sent[0].header("content-type").unwrap_or_default();
        let boundary = content_type
            .strip_prefix("multipart/form-data; boundary=")
            .unwrap_or_else(|| panic!("a multipart body: {content_type}"));
        let body = sent[0].body_text();
        for part in [
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"payload_json\""),
            "\"content\":\"hi\"".to_owned(),
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"files[0]\"; \
                 filename=\"a.png\""
            ),
            "Content-Type: image/png".to_owned(),
        ] {
            assert!(body.contains(&part), "{part}: {body}");
        }
        assert!(
            sent[0]
                .body
                .windows(picture.len())
                .any(|at| at == picture.as_slice()),
            "the content's bytes are there whole"
        );
        // What the key was used for says how much was sent, never what.
        let uses = items.uses(&item).unwrap();
        assert_eq!(uses.len(), 1);
        assert_eq!(
            uses[0].what,
            format!("POST /v1/upload ({} bytes)", sent[0].body.len())
        );

        // Content this workspace keeps for no app it asks as is refused, and nothing is sent.
        let mut unkept = call.clone();
        if let Some(part) = unkept.body_parts.get_mut(1)
            && let Some(content) = &mut part.content
        {
            content.app = "so.tilo.discord".into();
        }
        let (lookup, _) = answering(&["127.0.0.1"]);
        let refused = send_through(
            Destinations::Any,
            lookup,
            &mut items,
            unkept,
            "Scout",
            None,
            UsedFor::default(),
            &keeps,
        )
        .await;
        assert_eq!(refused, Err(NOT_KEPT.to_owned()));
        assert!(service.drain_requests().is_empty(), "nothing was sent");
    }

    /// What a call sends is bounded by what the workspace's store keeps, not by what one inline
    /// request holds: twelve megabytes of content ride one request whole, while text stays
    /// bounded to what a call sends.
    #[tokio::test]
    async fn a_call_sends_twelve_megabytes_of_content_whole() {
        use test_http::{Reply, Responder};

        let service =
            Responder::handle(|request| Some(Reply::status(200).body(request.body.len().to_string())));
        let bytes = vec![0x5a_u8; 12 * 1024 * 1024];
        let hash = blake3::hash(&bytes).to_hex().to_string();
        let (mut items, mut call) = kept_for(&service.url());
        call.url = format!("{}/v1/upload", service.url());
        call.method = "POST".to_owned();
        call.body_content = Some(ContentRef {
            app: "so.tilo.chat".into(),
            hash: hash.clone(),
            filename: "discord.zip".into(),
            content_type: "application/zip".into(),
        });
        let keeps = |app: &str, asked: &str| {
            (app == "so.tilo.chat" && asked == hash).then(|| bytes.clone())
        };
        let (lookup, _) = answering(&["127.0.0.1"]);
        let answer = send_through(
            Destinations::Any,
            lookup,
            &mut items,
            call,
            "Scout",
            None,
            UsedFor::default(),
            &keeps,
        )
        .await
        .unwrap();
        assert_eq!(answer["status"], 200);
        assert_eq!(
            answer["body"],
            Value::String(bytes.len().to_string()),
            "the service saw every byte"
        );
        let sent = service.drain_requests();
        assert_eq!(sent.len(), 1, "one request");
        assert_eq!(sent[0].header("content-type"), Some("application/zip"));
        assert_eq!(sent[0].body, bytes, "the content's bytes are there whole");

        // Text is still bounded to what one call sends.
        let (mut items, mut call) = kept_for(&service.url());
        call.url = format!("{}/v1/upload", service.url());
        call.method = "POST".to_owned();
        call.body = "a".repeat(MAX_SENT_BYTES.saturating_add(1));
        let (lookup, _) = answering(&["127.0.0.1"]);
        let refused = send_through(
            Destinations::Any,
            lookup,
            &mut items,
            call,
            "Scout",
            None,
            UsedFor::default(),
            &|_, _| None,
        )
        .await;
        assert_eq!(refused, Err(TOO_MUCH.to_owned()));
        assert!(service.drain_requests().is_empty(), "nothing was sent");
    }

    /// The name of a call is looked up once, every address the lookup
    /// answers is checked, and the connection goes to exactly what was
    /// checked. A name that answers an address inside this server's own
    /// network, alone, before or after a public one, is refused before
    /// anything is dialled; where the operator lets the server call its own
    /// network, the address the one lookup answered is the one dialled.
    #[tokio::test]
    async fn a_name_is_looked_up_once_and_dialled_only_where_every_answer_was_checked() {
        let quiet = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        quiet.set_nonblocking(true).unwrap();
        let service = format!(
            "https://api.example.test:{}",
            quiet.local_addr().unwrap().port()
        );
        let (mut items, call) = kept_for(&service);
        for answers in [
            vec!["127.0.0.1"],
            vec!["10.0.0.7"],
            vec!["169.254.169.254"],
            vec!["::1"],
            vec!["93.184.216.34", "127.0.0.1"],
            vec!["127.0.0.1", "93.184.216.34"],
            vec!["2606:2800:220:1:248:1893:25c8:1946", "fd00::1"],
        ] {
            let (lookup, asked) = answering(&answers);
            let refused = send_through(
                Destinations::Public,
                lookup,
                &mut items,
                call.clone(),
                "Scout",
                None,
                UsedFor::default(),
                &no_content,
            )
            .await;
            assert_eq!(refused, Err(INSIDE.to_owned()), "{answers:?}");
            assert_eq!(asked.load(Ordering::SeqCst), 1, "{answers:?}: one lookup");
            assert!(
                matches!(quiet.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
                "{answers:?}: nothing was dialled"
            );
        }
        // A name that answers nothing is not dialled either.
        let (lookup, asked) = answering(&[]);
        let nowhere = send_through(
            Destinations::Public,
            lookup,
            &mut items,
            call,
            "Scout",
            None,
            UsedFor::default(),
            &no_content,
        )
        .await;
        assert_eq!(nowhere, Err(UNREACHED.to_owned()));
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        assert!(quiet.accept().is_err(), "nothing was dialled");

        // Where the operator lets the server call its own network, the one
        // lookup's answer is what is dialled. What listens there hangs up.
        let listening = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let service = format!(
            "https://api.example.test:{}",
            listening.local_addr().unwrap().port()
        );
        let reached = std::thread::spawn(move || listening.accept().is_ok());
        let (mut items, call) = kept_for(&service);
        let (lookup, asked) = answering(&["127.0.0.1"]);
        let cut = send_through(
            Destinations::Any,
            lookup,
            &mut items,
            call,
            "Scout",
            None,
            UsedFor::default(),
            &no_content,
        )
        .await;
        assert_eq!(cut, Err(UNREACHED.to_owned()), "no refusal says more");
        assert_eq!(asked.load(Ordering::SeqCst), 1, "one lookup");
        assert!(
            reached.join().unwrap(),
            "the connection went to the address the lookup answered"
        );
    }

    /// A call's key is written down as used for the call's method, path and
    /// the size of what it sent, under the use's word and with how its person
    /// allowed it: never with the address's query, and never with what was
    /// sent.
    #[tokio::test]
    async fn a_call_is_written_down_by_its_method_and_path_alone() {
        let listening = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let service = format!(
            "https://api.example.test:{}",
            listening.local_addr().unwrap().port()
        );
        let reached = std::thread::spawn(move || listening.accept().is_ok());
        let (mut items, mut call) = kept_for(&service);
        let item = item_of(&call).unwrap();
        call.url = format!("{service}/v1/charges?account=7&note=kept-nowhere#end-kept-nowhere");
        call.method = "post".to_owned();
        let body = "amount=12&memo=kept-nowhere";
        call.body = body.to_owned();
        let (lookup, _) = answering(&["127.0.0.1"]);
        let purpose = UsedFor {
            using: "tilo.vault/web-call",
            what: "what a caller says of it is not what is written",
            allowed: Some(vault_items::Allowance::Once),
        };
        let cut = send_through(
            Destinations::Any,
            lookup,
            &mut items,
            call,
            "Scout",
            None,
            purpose,
            &no_content,
        )
        .await;
        assert_eq!(cut, Err(UNREACHED.to_owned()));
        assert!(reached.join().unwrap(), "the call went out");
        let uses = items.uses(&item).unwrap();
        assert_eq!(uses.len(), 1);
        assert_eq!(uses[0].using, "tilo.vault/web-call");
        assert_eq!(
            uses[0].what,
            format!("POST /v1/charges ({} bytes)", body.len())
        );
        assert_eq!(uses[0].allowed, Some(vault_items::Allowance::Once));
        assert_eq!(uses[0].by, "Scout");
        let said = format!("{uses:?}");
        for unwritten in [KEY, "account=7", "amount=12", "kept-nowhere", "caller says"] {
            assert!(!said.contains(unwritten), "{unwritten}: {said}");
        }
    }

    fn call(url: &str) -> WebCall {
        serde_json::from_value(json!({
            "url": url,
            "key": format!("vault.item:{}", "0".repeat(32)),
        }))
        .unwrap()
    }

    /// Base64 as the standard's own examples spell it.
    #[test]
    fn base64_is_the_standards() {
        for (plain, padded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(plain.as_bytes(), STANDARD, true), padded.as_bytes());
            assert_eq!(
                base64(plain.as_bytes(), STANDARD, false),
                padded.trim_end_matches('=').as_bytes()
            );
        }
        assert_eq!(base64(&[0xfb, 0xff], STANDARD, true), b"+/8=");
        assert_eq!(base64(&[0xfb, 0xff], URL_SAFE, false), b"-_8");
    }

    /// The key is struck out of an answer in every form a service that
    /// repeats what it was sent spells it in, and nothing else is touched.
    #[test]
    fn a_key_is_struck_in_every_form_it_was_sent_in() {
        let struck = Struck::of(&[KEY]);
        let encoded = String::from_utf8(percent(KEY.as_bytes(), true)).unwrap();
        assert_eq!(encoded, "made-up%20key%2F0123%2B456%3F789%26for%3Dtests");
        let forms = [
            KEY.to_owned(),
            encoded.clone(),
            String::from_utf8(percent(KEY.as_bytes(), false)).unwrap(),
            encoded.replace("%20", "+"),
            String::from_utf8(base64(KEY.as_bytes(), STANDARD, true)).unwrap(),
            String::from_utf8(base64(KEY.as_bytes(), URL_SAFE, false)).unwrap(),
            hex::encode(KEY),
            hex::encode(KEY).to_ascii_uppercase(),
            KEY.replace('/', "\\/").replace('&', "&amp;"),
            KEY.replace('&', "&amp;"),
        ];
        for form in &forms[..8] {
            let answer = format!("{{\"seen\": \"Bearer {form}\", \"other\": 7}}");
            let handed = struck.out_of(answer.as_bytes(), answer.len());
            assert_eq!(
                handed,
                format!("{{\"seen\": \"Bearer {WITHHELD}\", \"other\": 7}}"),
                "{form}"
            );
        }
        let page = format!("<p>{}</p>", forms[9]);
        assert_eq!(
            struck.out_of(page.as_bytes(), page.len()),
            format!("<p>{WITHHELD}</p>")
        );
        let quoted = serde_json::to_string(&format!("a \"{KEY}\" b")).unwrap();
        assert!(
            !struck
                .out_of(quoted.as_bytes(), quoted.len())
                .contains("0123")
        );
        let untouched = "nothing of the key here: made-up";
        assert_eq!(struck.out_of(untouched.as_bytes(), 1_000), untouched);
    }

    /// A key that stands across the cut of a long answer is struck whole,
    /// and one that begins past the cut is not handed back at all.
    #[test]
    fn a_key_across_the_cut_is_not_handed_back_by_its_beginning() {
        let struck = Struck::of(&[KEY]);
        let answer = format!("0123456789{KEY} and more {KEY}");
        for most in 0..answer.len() {
            let handed = struck.out_of(answer.as_bytes(), most);
            assert!(!handed.contains("made-up"), "cut at {most}: {handed}");
            assert!(handed.starts_with(&"0123456789"[..most.min(10)]));
        }
        assert_eq!(
            struck.out_of(answer.as_bytes(), 11),
            format!("0123456789{WITHHELD}")
        );
        assert_eq!(struck.longest(), hex::encode(KEY).len());
    }

    /// A basic sign-in's name and key are struck as they were sent
    /// together, and the key alone as well.
    #[test]
    fn a_basic_sign_in_is_struck_as_it_was_sent() {
        let pair = format!("ada:{KEY}");
        let struck = Struck::of(&[KEY, pair.as_str()]);
        let sent = String::from_utf8(base64(pair.as_bytes(), STANDARD, true)).unwrap();
        let answer = format!("authorization: Basic {sent}; key {KEY}");
        assert_eq!(
            struck.out_of(answer.as_bytes(), answer.len()),
            format!("authorization: Basic {WITHHELD}; key {WITHHELD}")
        );
    }

    /// A key is sent to an https address on the internet and nowhere else:
    /// not in the clear, not with a sign-in in the address, not to this
    /// server's own network unless its operator lets it call there, and
    /// then in the clear only to a literal address.
    #[test]
    fn a_key_is_sent_to_an_https_address_and_to_no_other() {
        let public = Destinations::Public;
        let sent = address(
            &call("https://api.example.test/v1/balance?page=2#top"),
            public,
        )
        .unwrap();
        assert_eq!(sent.as_str(), "https://api.example.test/v1/balance?page=2");
        for refused in [
            "http://api.example.test/v1",
            "https://ada:secret@api.example.test/v1",
            "https://ada@api.example.test/v1",
            "ftp://api.example.test/file",
            "file:///etc/passwd",
            "https://127.0.0.1/v1",
            "https://10.0.0.7/v1",
            "https://[::1]/v1",
            "http://127.0.0.1:8080/v1",
            "https://api.example.test:22/v1",
            "not an address",
            "",
        ] {
            assert!(address(&call(refused), public).is_err(), "{refused}");
        }
        let any = Destinations::Any;
        assert!(address(&call("http://127.0.0.1:8080/v1"), any).is_ok());
        assert!(address(&call("http://[::1]:8080/v1"), any).is_ok());
        assert!(address(&call("https://10.0.0.7/v1"), any).is_ok());
        for refused in ["http://api.example.test/v1", "http://localhost:8080/v1"] {
            assert!(address(&call(refused), any).is_err(), "{refused}");
        }
        let long = format!("https://api.example.test/{}", "a".repeat(MAX_ADDRESS_BYTES));
        assert!(address(&call(&long), public).is_err());
    }

    /// The vault is asked for the origin of the address, whatever else the
    /// address says.
    #[test]
    fn the_place_a_call_asks_the_vault_for_is_the_origin_of_its_address() {
        for (url, origin) in [
            (
                "https://api.example.test/v1/balance?x=1",
                "https://api.example.test",
            ),
            ("https://API.Example.Test:443/", "https://api.example.test"),
            (
                "https://api.example.test:8443/v1",
                "https://api.example.test:8443",
            ),
            (
                "https://ada@api.example.test/v1",
                "https://api.example.test",
            ),
            ("http://127.0.0.1:8080/v1", "http://127.0.0.1:8080"),
        ] {
            let (_, kind, places) = wanted(&call(url)).unwrap();
            assert_eq!(kind, vault_items::ItemKind::Key);
            assert_eq!(places, vec![Place::parse(origin).unwrap()], "{url}");
        }
        assert!(wanted(&call("mailto:ada@example.test")).is_err());
        assert!(wanted(&call("nowhere")).is_err());
    }

    /// The headers that frame a request, sign it in or carry the key are
    /// not the agent's to set; the rest are sent as named.
    #[test]
    fn the_headers_that_sign_a_request_in_are_not_the_agents() {
        let with = |name: &str, value: &str| {
            let mut asked = call("https://api.example.test/v1");
            asked.headers.insert(name.to_owned(), value.to_owned());
            asked
        };
        let named = KeyIn::parse("header:X-Api-Key").unwrap();
        for own in [
            "Host",
            "Authorization",
            "cookie",
            "Content-Length",
            "Transfer-Encoding",
            "X-Forwarded-Host",
            "X-HTTP-Method-Override",
            "x-api-key",
            " X-Api-Key ",
        ] {
            assert!(headers(&with(own, "x"), &named).is_err(), "{own}");
        }
        assert!(headers(&with("X-Api-Key", "x"), &KeyIn::Bearer).is_ok());
        assert!(headers(&with("Accept", "line\r\nbreak: x"), &KeyIn::Bearer).is_err());
        assert!(headers(&with("not a name", "x"), &KeyIn::Bearer).is_err());
        let sent = headers(&with("Accept", "application/json"), &named).unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0.as_str(), "accept");

        let query = KeyIn::parse("query:api_key").unwrap();
        let taken = Url::parse("https://api.example.test/v1?api_key=mine").unwrap();
        assert!(rides_alone(&taken, &query).is_err());
        let free = Url::parse("https://api.example.test/v1?page=2").unwrap();
        assert!(rides_alone(&free, &query).is_ok());
        assert!(rides_alone(&taken, &KeyIn::Bearer).is_ok());

        assert!(
            method(&call("https://api.example.test")).is_ok_and(|method| method == Method::GET)
        );
        let mut odd = call("https://api.example.test");
        odd.method = "CONNECT".into();
        assert!(method(&odd).is_err());
        odd.method = "delete".into();
        assert!(method(&odd).is_ok_and(|method| method == Method::DELETE));
    }

    /// A header that carries the key prints nothing of it.
    #[test]
    fn a_header_that_carries_the_key_prints_nothing_of_it() {
        let (name, value) = key_header("X-Api-Key", "made-up-key-0123456789").unwrap();
        assert_eq!(name.as_str(), "x-api-key");
        assert!(!format!("{value:?}").contains("made-up"));
        assert!(key_header("X-Api-Key", "two\nlines").is_err());
    }

    /// Every unusual IPv4 spelling, bracketed or IPv4-mapped IPv6 literal,
    /// trailing-dot literal, port and credentialed address is judged by the
    /// shared literal-address guard.
    #[test]
    fn literal_addresses_are_refused_in_every_spelling_and_plain_only_when_let() {
        for refused in [
            "https://127.0.0.1/",
            "https://127.0.0.1./",
            "https://2130706433/",
            "https://0x7f.1/",
            "https://0177.0.0.1/",
            "https://127.1/",
            "https://0/",
            "https://169.254.169.254/latest/meta-data/",
            "https://10.0.0.1:8443/",
            "https://172.16.0.1/",
            "https://192.168.1.1/",
            "https://[::1]:8443/",
            "https://[fe80::1]/",
            "https://[fc00::1]/",
            "https://[::ffff:127.0.0.1]/",
            "https://[::ffff:10.0.0.7]:8443/",
            "https://user:pass@127.0.0.1/",
            "https://user:pass@[::1]:8443/",
        ] {
            assert!(
                address(&call(refused), Destinations::Public).is_err(),
                "{refused}"
            );
        }
        for plain_inside in [
            "http://127.0.0.1:8080/v1",
            "http://127.0.0.1./v1",
            "http://2130706433:8080/v1",
            "http://0x7f.1:8080/v1",
            "http://[::1]:8080/v1",
            "http://[::ffff:127.0.0.1]:8080/v1",
            "http://10.0.0.7:8080/v1",
        ] {
            assert!(
                address(&call(plain_inside), Destinations::Any).is_ok(),
                "{plain_inside}"
            );
        }
        for not_plain in [
            "http://1.1.1.1/v1",
            "http://1.1.1.1./v1",
            "http://[::ffff:1.1.1.1]/v1",
            "http://[2606:4700:4700::1111]/v1",
            "http://example.com./v1",
            "http://localhost./v1",
            "blob:http://127.0.0.1/x",
            "redis://127.0.0.1/",
            "file://127.0.0.1/etc/passwd",
        ] {
            assert!(
                address(&call(not_plain), Destinations::Any).is_err(),
                "{not_plain}"
            );
        }
        let (items, mut with_key) = kept_for("https://api.example.test");
        with_key.url = "https://api.example.test/v1".to_owned();
        assert!(
            sendable(&with_key, Destinations::Public, &items).is_ok(),
            "control row must pass sendable with a vault key"
        );
        for non_http in [
            "blob:http://127.0.0.1/x",
            "redis://127.0.0.1/",
            "file://127.0.0.1/etc/passwd",
        ] {
            with_key.url = non_http.to_owned();
            assert!(
                sendable(&with_key, Destinations::Public, &items).is_err(),
                "{non_http} (public)"
            );
            assert!(
                sendable(&with_key, Destinations::Any, &items).is_err(),
                "{non_http} (any)"
            );
        }
    }

    /// Every header in the shared withheld set and the web-call-specific set
    /// is refused, in any case, and credential response headers stay stripped.
    #[test]
    fn every_shared_and_web_call_withheld_header_is_refused() {
        let with = |name: &str, value: &str| {
            let mut asked = call("https://api.example.test/v1");
            asked.headers.insert(name.to_owned(), value.to_owned());
            asked
        };
        for refused in [
            // Shared with the fetch proxy.
            "connection",
            "content-length",
            "expect",
            "forwarded",
            "host",
            "keep-alive",
            "proxy-authorization",
            "proxy-connection",
            "te",
            "trailer",
            "transfer-encoding",
            "upgrade",
            "x-real-ip",
            "x-forwarded-for",
            "X-Forwarded-Proto",
            "Host",
            // Web-call-specific extras.
            "authorization",
            "cookie",
            "x-http-method",
            "x-http-method-override",
            "x-method-override",
            "x-original-url",
            "x-rewrite-url",
            "X-Rewrite-URL",
        ] {
            assert!(
                headers(&with(refused, "v"), &KeyIn::Bearer).is_err(),
                "{refused} must be refused"
            );
        }
        // Credential response headers remain stripped by `not_handed_back`.
        let mut response = HeaderMap::new();
        for name in [
            "authorization",
            "proxy-authenticate",
            "proxy-authorization",
            "set-cookie",
            "set-cookie2",
            "content-type",
        ] {
            response.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_static("ok"),
            );
        }
        let answered = handed_back(&response, &Struck::of(&[KEY]));
        for stripped in [
            "authorization",
            "proxy-authenticate",
            "proxy-authorization",
            "set-cookie",
            "set-cookie2",
        ] {
            assert!(!answered.contains_key(stripped), "{stripped}");
        }
        assert_eq!(answered.get("content-type"), Some(&json!("ok")));
    }
}
