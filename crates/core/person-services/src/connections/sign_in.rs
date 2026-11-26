//! Signing in to a connected service by the service's own flow.
//!
//! A service that wants a sign-in says where its sign-ins are issued (its
//! protected-resource metadata), and the issuer says where a person signs
//! in and where a code is exchanged (its authorization-server metadata).
//! This server makes itself known there as a public client, sends the
//! person's browser to the issuer with a proof key, and exchanges the code
//! the browser comes back with. Every address the service or its issuer
//! names is data from outside and is asked through the one guarded way
//! ([`super::wire`]), like the service itself; no redirect is followed at
//! any step.
//!
//! What the issuer hands out is kept in the person's vault and nowhere
//! else: the token the service takes belongs to the service's own address,
//! the token that renews it to the address it is renewed at, and each is
//! handed out only there ([`VaultItems::use_as`]). A connection keeps which
//! items they are. Nothing here logs a token, a code or an address that
//! carries one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use accounts::vault_items::{ItemId, ItemKind, NewItem, Place};

use crate::VaultAt;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};
use tilo_lock::MutexRecover as _;
use tilo_net::{HeaderValue, Url};

use super::address::{Address, reached_by, site_of};
use super::said;
use super::wire::{Ask, Destinations, Problem, Reach, Wire, is_non_public_literal};

/// How long a step of a sign-in may take the service.
const STEP_WAIT: Duration = Duration::from_secs(20);
/// The most of one answer of a sign-in that is read.
const MAX_STEP_BYTES: usize = 256 * 1024;
/// How long a person has to come back from the service's sign-in.
const SIGN_IN_TIME_MS: u64 = 10 * 60 * 1000;
/// The longest address a browser is sent back to.
const MAX_RETURN_BYTES: usize = 2_048;
/// The most sign-ins begun and not finished that are remembered.
const MAX_BEGUN: usize = 1_024;

pub(crate) const NO_WAY: &str = "The service does not say how to sign in to it";
pub(crate) const BEGIN_AGAIN: &str =
    "That sign-in was not begun here or took too long. Begin it again";
const NOT_SIGNED_IN: &str = "The service did not sign you in";
const NO_VAULT: &str = "The vault is unavailable";

/// How a service says its person signs in. Nothing of it is secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Way {
    /// Who issues its sign-ins.
    pub(crate) issuer: String,
    /// Where the person's browser goes to sign in.
    pub(crate) authorize: String,
    /// Where a code is exchanged, and a sign-in renewed.
    pub(crate) token: String,
    /// Where this server makes itself known, when the issuer takes that.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) register: Option<String>,
    /// What the service wants to be allowed, when it said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) scope: Option<String>,
    /// The name the service gives itself there, when it gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
}

impl Way {
    /// The site the person signs in at, by name.
    pub(crate) fn site(&self) -> Option<String> {
        tilo_failure::ignored!(
            "a way to sign in whose address does not read names no site to show",
            Url::parse(&self.authorize).ok()
        )
        .map(|url| site_of(&url))
    }
}

/// What this server is known as at an issuer, for one place a browser comes
/// back to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Client {
    pub(crate) id: String,
    pub(crate) return_to: String,
}

/// Which items of the person's vault a connection signs in with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Tokens {
    /// The item the service takes.
    pub(crate) access: String,
    /// The item that renews it, when the issuer gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) renewal: Option<String>,
    /// When the service stops taking the first, when the issuer said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) expires_ms: Option<u64>,
}

/// A sign-in begun and not yet finished. It is kept in memory alone: its
/// proof key is a secret of the minutes the sign-in takes.
struct Begun {
    state: String,
    verifier: String,
    until_ms: u64,
}

static BEGUN: Mutex<BTreeMap<(PathBuf, String), Begun>> = Mutex::new(BTreeMap::new());

/// `bytes` in the URL-safe base64 alphabet, unpadded.
pub(crate) fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut text = String::with_capacity(bytes.len().div_ceil(3).saturating_mul(4));
    for chunk in bytes.chunks(3) {
        let mut triple = [0_u8; 3];
        for (place, byte) in triple.iter_mut().zip(chunk) {
            *place = *byte;
        }
        let [a, b, c] = triple;
        let sextets = [
            a >> 2,
            ((a & 0x03) << 4) | (b >> 4),
            ((b & 0x0f) << 2) | (c >> 6),
            c & 0x3f,
        ];
        for sextet in sextets.iter().take(chunk.len().saturating_add(1)) {
            if let Some(letter) = ALPHABET.get(usize::from(*sextet)) {
                text.push(char::from(*letter));
            }
        }
    }
    text
}

/// A fresh secret no one can guess, as text a URL carries.
fn unguessable() -> String {
    base64url(&rand::random::<[u8; 32]>())
}

/// `pairs` as a form a token endpoint reads.
fn form(pairs: &[(&str, &str)]) -> Vec<u8> {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    let digit = |nibble: u8| {
        DIGITS
            .get(usize::from(nibble))
            .map_or('0', |digit| char::from(*digit))
    };
    let mut encoded = String::new();
    for (name, value) in pairs {
        if !encoded.is_empty() {
            encoded.push('&');
        }
        for (part, last) in [(name, false), (value, true)] {
            for byte in part.bytes() {
                if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
                    encoded.push(char::from(byte));
                } else {
                    encoded.push('%');
                    encoded.push(digit(byte >> 4));
                    encoded.push(digit(byte & 0x0f));
                }
            }
            if !last {
                encoded.push('=');
            }
        }
    }
    encoded.into_bytes()
}

/// The address `named` spells, when it is one a step of a sign-in may be
/// asked at or a browser sent to: `https`, or plain only for a literal
/// address on this server's own network where its operator lets it call
/// there; with no name and password in it and no fragment.
fn endpoint(named: &str, destinations: Destinations) -> Option<Url> {
    let url = Url::parse(named).ok()?;
    let plain_inside =
        url.scheme() == "http" && destinations == Destinations::Any && is_non_public_literal(&url);
    (named.len() <= MAX_RETURN_BYTES
        && (url.scheme() == "https" || plain_inside)
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none())
    .then_some(url)
}

/// Ask `url` for the JSON object it answers, with `post` as a form or as
/// JSON when there is something to send.
async fn asked(
    wire: &Wire,
    url: Url,
    post: Option<(&'static str, Vec<u8>)>,
) -> Result<(u16, Map<String, Value>), Problem> {
    let mut headers = vec![("accept", HeaderValue::from_static("application/json"))];
    let post = post.map(|(kind, body)| {
        headers.push(("content-type", HeaderValue::from_static(kind)));
        body
    });
    let sent = wire
        .send(Ask {
            url,
            post,
            headers,
            wait: STEP_WAIT,
            most: MAX_STEP_BYTES,
            whole: None,
        })
        .await?;
    let answer = tilo_failure::ignored!(
        "an answer that is no JSON is judged by its status in the lines below",
        serde_json::from_slice::<Value>(&sent.body).ok()
    );
    match answer {
        Some(Value::Object(answer)) => Ok((sent.status, answer)),
        _ if (200..300).contains(&sent.status) => Err(Problem::Unreadable),
        _ => Ok((sent.status, Map::new())),
    }
}

/// The value of `name` in what a service said of where to sign in.
fn challenged(challenge: &str, name: &str) -> Option<String> {
    let after = challenge.split_once(&format!("{name}=\""))?.1;
    Some(after.split_once('"')?.0.to_owned())
}

fn text<'a>(said: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    said.get(name).and_then(Value::as_str)
}

/// Where the service at `address` describes itself as something to sign in
/// to: the place its answer named, when that is on the service's own site,
/// then the places every such service is asked at.
fn described_at(address: &Address, challenge: Option<&str>) -> Vec<Url> {
    let url = address.url();
    let mut places = Vec::new();
    let named = challenge
        .and_then(|challenge| challenged(challenge, "resource_metadata"))
        .and_then(|named| {
            tilo_failure::ignored!(
                "a place that does not read as an address is not asked, and the usual places are",
                Url::parse(&named).ok()
            )
        })
        .filter(|named| named.origin() == url.origin() && named.query().is_none());
    places.extend(named);
    let path = url.path().trim_end_matches('/');
    for well_known in [
        format!("/.well-known/oauth-protected-resource{path}"),
        "/.well-known/oauth-protected-resource".to_owned(),
    ] {
        let mut place = url.clone();
        place.set_path(&well_known);
        if !places.contains(&place) {
            places.push(place);
        }
    }
    places
}

/// Where the issuer `issuer` describes how it signs people in.
fn issuer_described_at(issuer: &Url) -> Vec<Url> {
    let path = issuer.path().trim_end_matches('/');
    let mut places = Vec::new();
    for well_known in [
        format!("/.well-known/oauth-authorization-server{path}"),
        format!("/.well-known/openid-configuration{path}"),
        format!("{path}/.well-known/openid-configuration"),
    ] {
        let mut place = issuer.clone();
        place.set_path(&well_known);
        if !places.contains(&place) {
            places.push(place);
        }
    }
    places
}

/// Whether `resource`, the address a description is about, is the service
/// at `address`: that address, or one above it on the same site. A
/// description of something else is not this service's.
fn describes(resource: &str, address: &Address) -> bool {
    let Ok(resource) = Url::parse(resource) else {
        return false;
    };
    let (about, service) = (resource.path().trim_end_matches('/'), address.url().path());
    resource.origin() == address.url().origin()
        && resource.query().is_none()
        && service
            .strip_prefix(about)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// How the service at `address` says its person signs in, read after it
/// answered `challenge` to a request that carried no sign-in.
pub(crate) async fn discover(
    reach: &Reach,
    address: &Address,
    challenge: Option<&str>,
) -> Result<Way, Problem> {
    let wire = Wire::new(reach)?;
    let mut described = None;
    for place in described_at(address, challenge) {
        if let (200, said) = asked(&wire, place, None).await? {
            described = Some(said);
            break;
        }
    }
    let described = described.ok_or(Problem::Unreadable)?;
    if !text(&described, "resource").is_some_and(|resource| describes(resource, address)) {
        return Err(Problem::Unreadable);
    }
    let issuer = described
        .get("authorization_servers")
        .and_then(Value::as_array)
        .and_then(|issuers| issuers.first())
        .and_then(Value::as_str)
        .ok_or(Problem::Unreadable)?;
    let issuer_url = endpoint(issuer, reach.destinations)
        .filter(|issuer| issuer.query().is_none())
        .ok_or(Problem::NotCalled)?;
    let mut offers = None;
    for place in issuer_described_at(&issuer_url) {
        if let (200, said) = asked(&wire, place, None).await? {
            offers = Some(said);
            break;
        }
    }
    let offers = offers.ok_or(Problem::Unreadable)?;
    // What describes an issuer names that issuer, and the issuer proves a
    // code with a hashed key: a sign-in without that is not begun.
    let same_issuer = text(&offers, "issuer")
        .is_some_and(|named| named.trim_end_matches('/') == issuer.trim_end_matches('/'));
    let proves = offers
        .get("code_challenge_methods_supported")
        .and_then(Value::as_array)
        .is_some_and(|methods| methods.iter().any(|method| method == "S256"));
    if !same_issuer || !proves {
        return Err(Problem::Unreadable);
    }
    let named = |name: &str| {
        text(&offers, name)
            .filter(|named| endpoint(named, reach.destinations).is_some())
            .map(str::to_owned)
    };
    Ok(Way {
        issuer: issuer.to_owned(),
        authorize: named("authorization_endpoint").ok_or(Problem::Unreadable)?,
        token: named("token_endpoint").ok_or(Problem::Unreadable)?,
        register: named("registration_endpoint"),
        scope: challenge
            .and_then(|challenge| challenged(challenge, "scope"))
            .filter(|scope| scope.len() <= 1_024 && !scope.chars().any(char::is_control)),
        name: text(&described, "resource_name")
            .map(|name| said::line(name, said::MAX_NAME_CHARS))
            .filter(|name| !name.is_empty()),
    })
}

/// The address a browser comes back to, when `named` is one a person's own
/// app can catch: an `https` address, an address in a scheme of the app's
/// own (one with a dot in it, which no browser takes for the web), or a
/// plain address on the device itself.
fn return_address(named: &str) -> Option<Url> {
    let url = Url::parse(named).ok()?;
    let on_device = url.scheme() == "http"
        && matches!(reached_by(&url), Some("127.0.0.1" | "[::1]" | "localhost"));
    let apps_own = url.scheme().contains('.') && !url.cannot_be_a_base();
    (named.len() <= MAX_RETURN_BYTES
        && (url.scheme() == "https" || on_device || apps_own)
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && url.query().is_none())
    .then_some(url)
}

/// Make this server known to the issuer of `way` as a public client whose
/// browser comes back to `return_to`.
async fn register(reach: &Reach, way: &Way, return_to: &str) -> Result<Client, String> {
    let register = way
        .register
        .as_deref()
        .and_then(|register| endpoint(register, reach.destinations))
        .ok_or(NO_WAY)?;
    let known_as = json!({
        "client_name": "Tilo",
        "redirect_uris": [return_to],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    let wire = Wire::new(reach).map_err(|_| NO_WAY)?;
    let body = serde_json::to_vec(&known_as).map_err(|_| NO_WAY)?;
    let answer = asked(&wire, register, Some(("application/json", body))).await;
    match answer {
        Ok((200..=299, said)) => text(&said, "client_id")
            .filter(|id| !id.is_empty() && id.len() <= 512 && !id.chars().any(char::is_control))
            .map(|id| Client {
                id: id.to_owned(),
                return_to: return_to.to_owned(),
            })
            .ok_or_else(|| NO_WAY.to_owned()),
        _ => Err(NO_WAY.to_owned()),
    }
}

/// Begin a sign-in to the connection `id` of the person at `root`, whose
/// service is at `address` and signs in as `way` says: the address the
/// person's browser opens, until when the sign-in can be finished, and what
/// this server is known as there, for the connection to keep.
pub(crate) async fn begin(
    (root, id): (&Path, &str),
    reach: &Reach,
    address: &Address,
    way: &Way,
    known: Option<&Client>,
    return_to: &str,
) -> Result<(String, u64, Client), String> {
    let return_to = return_address(return_to).ok_or(
        "Where the browser comes back to is an https address, or an address your own app catches",
    )?;
    let client = match known.filter(|known| known.return_to == return_to.as_str()) {
        Some(known) => known.clone(),
        None => register(reach, way, return_to.as_str()).await?,
    };
    let mut open = endpoint(&way.authorize, reach.destinations).ok_or(NO_WAY)?;
    let (state, verifier) = (unguessable(), unguessable());
    let challenge = base64url(&Sha256::digest(verifier.as_bytes()));
    {
        let mut asking = open.query_pairs_mut();
        asking
            .append_pair("response_type", "code")
            .append_pair("client_id", &client.id)
            .append_pair("redirect_uri", &client.return_to)
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("resource", address.as_str());
        if let Some(scope) = &way.scope {
            asking.append_pair("scope", scope);
        }
    }
    let now = tilo_clock::unix_now_ms();
    let until_ms = now.saturating_add(SIGN_IN_TIME_MS);
    let mut begun = BEGUN.lock_recover();
    begun.retain(|_, begun| begun.until_ms > now);
    if begun.len() >= MAX_BEGUN {
        drop(begun);
        return Err("Too many sign-ins are under way. Try again in a few minutes".into());
    }
    begun.insert(
        (root.to_path_buf(), id.to_owned()),
        Begun {
            state,
            verifier,
            until_ms,
        },
    );
    drop(begun);
    Ok((open.into(), until_ms, client))
}

/// Whether `a` and `b` are the same, taking as long whichever they are.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0_u8, |differing, (a, b)| differing | (a ^ b))
            == 0
}

/// What an issuer handed out, on its way into the vault.
struct Issued {
    access: String,
    renewal: Option<String>,
    expires_ms: Option<u64>,
}

/// What `answer` of a token endpoint hands out, when it hands out a bearer
/// token.
fn issued(answer: &Map<String, Value>) -> Option<Issued> {
    let access = text(answer, "access_token").filter(|access| !access.is_empty())?;
    if !text(answer, "token_type").is_some_and(|kind| kind.eq_ignore_ascii_case("bearer")) {
        return None;
    }
    Some(Issued {
        access: access.to_owned(),
        renewal: text(answer, "refresh_token")
            .filter(|renewal| !renewal.is_empty())
            .map(str::to_owned),
        expires_ms: answer
            .get("expires_in")
            .and_then(Value::as_u64)
            .map(|seconds| tilo_clock::unix_now_ms().saturating_add(seconds.saturating_mul(1000))),
    })
}

fn place(of: &str) -> Result<Place, String> {
    Place::parse(of).map_err(|_| NO_VAULT.to_owned())
}

/// Keep what an issuer handed out in the workspace's `vault`: the token the
/// service takes for the service's own address, the one that renews it for
/// the address it is renewed at, and nothing anywhere else.
async fn keep(
    vault: VaultAt<'_>,
    address: &Address,
    way: &Way,
    issued: Issued,
) -> Result<Tokens, String> {
    let access_at = place(address.as_str())?;
    let renewal_at = place(&way.token)?;
    vault
        .changing(|items| {
            let access = items.add(NewItem::new(
                ItemKind::Key,
                vec![access_at],
                String::new(),
                issued.access,
            ))?;
            let renewal = match issued.renewal {
                None => None,
                Some(renewal) => Some(
                    items
                        .add(NewItem::new(
                            ItemKind::Key,
                            vec![renewal_at],
                            String::new(),
                            renewal,
                        ))?
                        .id
                        .as_str()
                        .to_owned(),
                ),
            };
            Ok(Tokens {
                access: access.id.as_str().to_owned(),
                renewal,
                expires_ms: issued.expires_ms,
            })
        })
        .await
        .map_err(|error| {
            tracing::warn!(%error, "a sign-in could not be kept in the workspace's vault");
            NO_VAULT.to_owned()
        })
}

/// Take the items `tokens` names out of the workspace's `vault`. An item
/// that is gone already is gone.
pub(crate) async fn forget(vault: VaultAt<'_>, tokens: &Tokens) {
    let forgotten = vault
        .changing(|items| {
            for id in [Some(&tokens.access), tokens.renewal.as_ref()]
                .into_iter()
                .flatten()
            {
                if let Some(id) = ItemId::parse(id) {
                    items.remove(&id);
                }
            }
            Ok(())
        })
        .await;
    if let Err(error) = forgotten {
        tracing::warn!(%error, "a sign-in that is over could not be taken out of the vault");
    }
}

/// Finish the sign-in begun for the connection `id` of the person at
/// `root`, with the whole address the person's browser came back to:
/// exchange its code and keep what the issuer hands out in their vault.
/// A sign-in is finished once: whatever comes of it, it is begun again.
pub(crate) async fn finish(
    (root, id): (&Path, &str),
    vault: VaultAt<'_>,
    reach: &Reach,
    address: &Address,
    (way, client): (&Way, &Client),
    returned: &str,
) -> Result<Tokens, String> {
    let now = tilo_clock::unix_now_ms();
    let begun = BEGUN
        .lock_recover()
        .remove(&(root.to_path_buf(), id.to_owned()))
        .filter(|begun| begun.until_ms > now)
        .ok_or(BEGIN_AGAIN)?;
    let came_back = Url::parse(returned).map_err(|_| BEGIN_AGAIN)?;
    let expected = Url::parse(&client.return_to).map_err(|_| BEGIN_AGAIN)?;
    let said = |name: &str| {
        came_back
            .query_pairs()
            .find(|(said, _)| said == name)
            .map(|(_, value)| value.into_owned())
    };
    let here = came_back.scheme() == expected.scheme()
        && reached_by(&came_back) == reached_by(&expected)
        && came_back.port() == expected.port()
        && came_back.path() == expected.path();
    if !here || !said("state").is_some_and(|state| same(&state, &begun.state)) {
        return Err(BEGIN_AGAIN.to_owned());
    }
    // An issuer that says who it is must be the one this was begun with.
    if said("iss")
        .is_some_and(|issuer| issuer.trim_end_matches('/') != way.issuer.trim_end_matches('/'))
    {
        return Err(NOT_SIGNED_IN.to_owned());
    }
    let code = said("code")
        .filter(|_| said("error").is_none())
        .ok_or(NOT_SIGNED_IN)?;
    let token = endpoint(&way.token, reach.destinations).ok_or(NO_WAY)?;
    let exchange = form(&[
        ("grant_type", "authorization_code"),
        ("code", &code),
        ("redirect_uri", &client.return_to),
        ("client_id", &client.id),
        ("code_verifier", &begun.verifier),
        ("resource", address.as_str()),
    ]);
    let wire = Wire::new(reach).map_err(|_| NOT_SIGNED_IN)?;
    let answer = asked(
        &wire,
        token,
        Some(("application/x-www-form-urlencoded", exchange)),
    )
    .await;
    let issued = match &answer {
        Ok((200, answer)) => issued(answer),
        _ => None,
    };
    keep(vault, address, way, issued.ok_or(NOT_SIGNED_IN)?).await
}

/// What the service is shown as the connection's sign-in, handed out of
/// the workspace's `vault` for the service's own address and no other, and
/// written down there as used for `by`.
pub(crate) async fn bearer(
    vault: VaultAt<'_>,
    address: &Address,
    tokens: &Tokens,
    by: &str,
) -> Option<accounts::vault_items::Released> {
    let id = ItemId::parse(&tokens.access)?;
    let at = Place::parse(address.as_str()).ok()?;
    let released = vault
        .changing(|items| items.use_as(&id, ItemKind::Key, &[at], by))
        .await;
    match released {
        Ok(released) => Some(released),
        Err(error) => {
            tracing::info!(%error, "a connection's sign-in is not in its person's vault");
            None
        }
    }
}

/// Why a sign-in was not renewed.
pub(crate) enum NotRenewed {
    /// The issuer no longer takes it: its person signs in again.
    RanOut,
    /// The issuer could not be asked now; the sign-in stays as it is.
    NotNow(Problem),
}

/// Renew the sign-in `tokens` names, at the address `way` renews it at:
/// what the issuer hands out replaces what the workspace's `vault` kept.
pub(crate) async fn renew(
    vault: VaultAt<'_>,
    reach: &Reach,
    address: &Address,
    (way, client): (&Way, &Client),
    tokens: &Tokens,
    by: &str,
) -> Result<Tokens, NotRenewed> {
    let token =
        endpoint(&way.token, reach.destinations).ok_or(NotRenewed::NotNow(Problem::NotCalled))?;
    let at = Place::parse(&way.token).map_err(|_| NotRenewed::NotNow(Problem::NotCalled))?;
    let renewal_id = tokens
        .renewal
        .as_deref()
        .and_then(ItemId::parse)
        .ok_or(NotRenewed::RanOut)?;
    let renewal = vault
        .changing(|items| items.use_as(&renewal_id, ItemKind::Key, &[at], by))
        .await
        .map_err(|_| NotRenewed::RanOut)?;
    let wire = Wire::new(reach).map_err(NotRenewed::NotNow)?;
    let asking = form(&[
        ("grant_type", "refresh_token"),
        ("refresh_token", renewal.secret.as_str()),
        ("client_id", &client.id),
        ("resource", address.as_str()),
    ]);
    drop(renewal);
    let answer = asked(
        &wire,
        token,
        Some(("application/x-www-form-urlencoded", asking)),
    )
    .await
    .map_err(NotRenewed::NotNow)?;
    let issued = match &answer {
        (200, answer) => issued(answer).ok_or(NotRenewed::RanOut)?,
        (400 | 401 | 403, _) => return Err(NotRenewed::RanOut),
        _ => return Err(NotRenewed::NotNow(Problem::TurnedAway)),
    };
    // An issuer that hands out no new renewal keeps the one it had.
    let kept_renewal = issued.renewal.is_none();
    let mut renewed = keep(vault, address, way, issued)
        .await
        .map_err(|_| NotRenewed::NotNow(Problem::NoAnswer))?;
    let mut over = tokens.clone();
    if kept_renewal {
        renewed.renewal = over.renewal.take();
    }
    forget(vault, &over).await;
    Ok(renewed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key and its proof read as the flow's own examples do, and a form
    /// is spelled as a token endpoint reads it.
    #[test]
    fn a_proof_key_and_a_form_are_spelled_as_the_flow_spells_them() {
        // RFC 7636, appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            base64url(&Sha256::digest(verifier.as_bytes())),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(base64url(b""), "");
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
        assert_eq!(unguessable().len(), 43);
        assert_ne!(unguessable(), unguessable());
        assert_eq!(
            String::from_utf8(form(&[
                ("grant_type", "authorization_code"),
                ("code", "a b&c=d/é")
            ]))
            .unwrap(),
            "grant_type=authorization_code&code=a%20b%26c%3Dd%2F%C3%A9"
        );
        assert!(same("abc", "abc") && !same("abc", "abd") && !same("abc", "abcd"));
    }

    /// What a service says of where to sign in is read by name, and a
    /// description of another service is not this one's.
    #[test]
    fn what_a_service_says_of_its_sign_in_is_read_strictly() {
        let challenge = "Bearer realm=\"orchard\", resource_metadata=\"https://orchard.example/.well-known/oauth-protected-resource/mcp\", scope=\"trees:read\"";
        assert_eq!(
            challenged(challenge, "resource_metadata").as_deref(),
            Some("https://orchard.example/.well-known/oauth-protected-resource/mcp")
        );
        assert_eq!(
            challenged(challenge, "scope").as_deref(),
            Some("trees:read")
        );
        assert_eq!(challenged(challenge, "error"), None);

        let address =
            Address::parse("https://orchard.example/mcp/v1", Destinations::Public).unwrap();
        for resource in [
            "https://orchard.example/mcp/v1",
            "https://orchard.example/mcp",
            "https://orchard.example",
            "https://orchard.example/",
        ] {
            assert!(describes(resource, &address), "{resource}");
        }
        for other in [
            "https://elsewhere.example/mcp/v1",
            "http://orchard.example/mcp/v1",
            "https://orchard.example:8443/mcp/v1",
            "https://orchard.example/mcp/v10",
            "https://orchard.example/mc",
            "https://orchard.example/mcp/v1/deeper",
            "https://orchard.example/mcp/v1?x=1",
            "not an address",
        ] {
            assert!(!describes(other, &address), "{other}");
        }
        // The place a service names for its description is taken only on
        // the service's own site.
        let elsewhere = "Bearer resource_metadata=\"https://elsewhere.example/.well-known/oauth-protected-resource\"";
        let places = described_at(&address, Some(elsewhere));
        assert!(
            places
                .iter()
                .all(|place| reached_by(place) == Some("orchard.example")),
            "{places:?}"
        );
        assert_eq!(places.len(), 2);
        assert_eq!(described_at(&address, Some(challenge)).len(), 3);
    }

    /// A browser comes back to an address the person's own app catches,
    /// and a step is asked only at an address a server calls.
    #[test]
    fn where_a_browser_comes_back_to_and_where_a_step_is_asked() {
        for caught in [
            "https://app.example/signed-in",
            "sh.tilo.assistant://signed-in",
            "sh.tilo.assistant:/signed-in",
            "http://127.0.0.1:49152/signed-in",
            "http://localhost:49152/signed-in",
        ] {
            assert!(return_address(caught).is_some(), "{caught}");
        }
        for other in [
            "http://app.example/signed-in",
            "javascript:alert(1)",
            "file:///etc/passwd",
            "tilo://signed-in",
            "data:text/html,x",
            "https://ada:secret@app.example/signed-in",
            "https://app.example/signed-in#token",
            "https://app.example/signed-in?next=elsewhere",
            "",
        ] {
            assert!(return_address(other).is_none(), "{other}");
        }
        assert!(
            endpoint(
                "https://accounts.orchard.example/token",
                Destinations::Public
            )
            .is_some()
        );
        assert!(endpoint("http://127.0.0.1:8000/token", Destinations::Any).is_some());
        for other in [
            "http://accounts.orchard.example/token",
            "http://127.0.0.1:8000/token",
            "https://ada:secret@accounts.orchard.example/token",
            "https://accounts.orchard.example/token#x",
            "javascript:alert(1)",
            "blob:http://127.0.0.1/x",
            "redis://127.0.0.1/",
            "file://127.0.0.1/etc/passwd",
        ] {
            assert!(endpoint(other, Destinations::Public).is_none(), "{other}");
        }
        for non_http in [
            "blob:http://127.0.0.1/x",
            "redis://127.0.0.1/",
            "file://127.0.0.1/etc/passwd",
        ] {
            assert!(
                endpoint(non_http, Destinations::Any).is_none(),
                "{non_http} (any)"
            );
        }
    }
}
