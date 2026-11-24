//! The API Tester's engine: the HTTP requests a person asks the app to send, and the credentials
//! they sign in with, both kept where they belong.
//!
//! The app runs sandboxed with no network and no secret store of its own, so it asks here by
//! name. A request arrives as JSON the app wrote — the method, the address, its query pairs,
//! headers, body and the variables to substitute — and is answered as JSON the app reads back
//! into its own records: the status, the headers, and as much of the body as the app shows.
//!
//! A credential never enters the workspace. The app hands it here once (`keep_credential`) and
//! keeps only the reference it gets back; a send resolves it from this device's secret store, so
//! a copy of the workspace carries a reference and no secret.
//!
//! A send is a job because a server may be slow to answer and a person must be able to stop
//! waiting. Each job is one request on the device's network, bounded in time
//! ([`REQUEST_TIMEOUT`]) and in the body it keeps ([`MAX_SHOWN_BODY_BYTES`]; a binary body is
//! shown as a hex dump of its start, [`MAX_HEX_BYTES`]).

use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use base64::Engine as _;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC};
use serde::Deserialize;
use tilo_secrets::{PutOptions, SecretStore};


/// The longest one request may take. A server that never answers is a failure with a reason,
/// not a job that waits forever.
pub const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// The most of a response body the app shows (2 MiB): its pane lays the text out line by line,
/// so more would freeze the window. The pane says how much it shows.
pub const MAX_SHOWN_BODY_BYTES: usize = 2 * 1024 * 1024;

/// The most of a binary body shown as a hex dump (64 KiB).
pub const MAX_HEX_BYTES: usize = 64 * 1024;

/// Everything except the RFC 3986 §2.3 unreserved characters, as a query component is encoded.
const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// What the app asks for, one field per part of the request as the person wrote it. Every text
/// may still hold `{{variables}}`: the engine substitutes them, since a secret variable's value
/// is a thing this device keeps and the app never holds.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Ask {
    pub method: String,
    pub url: String,
    /// The query pairs to add, key and value as written.
    #[serde(default)]
    pub params: Vec<(String, String)>,
    /// The headers to send, each name and value as written.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// The variables of the environment in use: name, value, and the reference that names a
    /// secret value in this device's store (empty for one whose value is shared).
    #[serde(default)]
    pub vars: Vec<(String, String, String)>,
    /// How the request signs in: its scheme, the user name and header that are public, and the
    /// reference that names the credential in this device's store.
    #[serde(default)]
    pub auth_scheme: String,
    #[serde(default)]
    pub auth_username: String,
    #[serde(default)]
    pub auth_header: String,
    #[serde(default)]
    pub auth_reference: String,
    #[serde(default)]
    pub body_mode: BodyMode,
    #[serde(default)]
    pub body: String,
    /// The form pairs, sent when `body_mode` is `form`.
    #[serde(default)]
    pub form: Vec<(String, String)>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BodyMode {
    #[default]
    None,
    Json,
    Form,
    Raw,
}

/// A fully resolved request, with every `{{var}}` expanded and auth folded into the headers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outgoing {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

fn encode_component(text: &str) -> String {
    percent_encoding::utf8_percent_encode(text, COMPONENT).to_string()
}

/// Replace every `{{name}}` in `input` with its value; whitespace inside the braces is
/// tolerated, and an unknown name stays visible, so the person sees exactly which placeholder
/// did not resolve.
#[must_use]
pub fn substitute<S: std::hash::BuildHasher>(
    input: &str,
    vars: &std::collections::HashMap<String, String, S>,
) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some((before, after_open)) = rest.split_once("{{") {
        out.push_str(before);
        let Some((raw, after_close)) = after_open.split_once("}}") else {
            out.push_str("{{");
            out.push_str(after_open);
            return out;
        };
        if let Some(value) = vars.get(raw.trim()) {
            out.push_str(value);
        } else {
            out.push_str("{{");
            out.push_str(raw);
            out.push_str("}}");
        }
        rest = after_close;
    }
    out.push_str(rest);
    out
}

fn append_query(base: String, query: &[String]) -> String {
    if query.is_empty() {
        return base;
    }
    // Params must precede a fragment: a fragment is not sent to the server.
    let (target, fragment) = base
        .split_once('#')
        .map_or((base.as_str(), None), |(target, fragment)| {
            (target, Some(fragment))
        });
    let separator = if target.ends_with('?') || (target.contains('?') && target.ends_with('&')) {
        ""
    } else if target.contains('?') {
        "&"
    } else {
        "?"
    };
    let mut url = format!("{target}{separator}{}", query.join("&"));
    if let Some(fragment) = fragment {
        url.push('#');
        url.push_str(fragment);
    }
    url
}

/// The store `app`'s credentials are kept in on this device.
fn store(app: &str) -> SecretStore {
    SecretStore::for_app(app)
}

/// Keep `value` as a credential on this device, answering the reference the workspace holds
/// instead. The reference is random: two credentials never share one.
///
/// # Errors
///
/// The device has no secret store it may write to.
pub fn keep_credential(app: &str, value: &str) -> Result<String, String> {
    let reference = fresh(job_counter());
    store(app)
        .put_blocking(&reference, value.as_bytes(), PutOptions::default())
        .map_err(|error| format!("this device keeps no credentials: {error}"))?;
    Ok(reference)
}

/// Forget the credential `reference` names on this device. A reference this device has no
/// credential for is forgotten already.
///
/// # Errors
///
/// The store could not be written.
pub fn forget_credential(app: &str, reference: &str) -> Result<(), String> {
    store(app)
        .delete_blocking(reference)
        .map_err(|error| format!("this device's credentials were not written: {error}"))
}

fn credential(app: &str, reference: &str) -> Option<String> {
    let secret = store(app).get_blocking(reference).ok()??;
    String::from_utf8(secret.expose().to_vec()).ok()
}

/// What a request asks for, resolved into what hits the wire: the query string, the headers
/// (auth and a body's content type folded in) and the body.
///
/// # Errors
///
/// A credential the workspace names is not on this device.
pub fn build_outgoing(app: &str, ask: &Ask) -> Result<Outgoing, String> {
    let mut values = std::collections::HashMap::new();
    for (key, shared, reference) in &ask.vars {
        let value = if reference.is_empty() {
            shared.clone()
        } else {
            credential(app, reference).ok_or_else(|| {
                format!("`{key}` is a secret this device holds no value for: enter it again")
            })?
        };
        values.insert(key.trim().to_owned(), value);
    }
    let sub = |text: &str| substitute(text, &values);

    let query: Vec<String> = ask
        .params
        .iter()
        .filter(|(key, _)| !sub(key).trim().is_empty())
        .map(|(key, value)| {
            format!(
                "{}={}",
                encode_component(&sub(key)),
                encode_component(&sub(value))
            )
        })
        .collect();
    let url = append_query(sub(&ask.url), &query);

    let mut headers: Vec<(String, String)> = ask
        .headers
        .iter()
        .filter(|(name, _)| !sub(name).trim().is_empty())
        .map(|(name, value)| (sub(name), sub(value)))
        .collect();

    let has_content_type = |headers: &[(String, String)]| {
        headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("content-type"))
    };

    match ask.auth_scheme.as_str() {
        "bearer" | "basic" | "api_key" if !ask.auth_reference.is_empty() => {
            let secret = credential(app, &ask.auth_reference).ok_or_else(|| {
                "this request's sign-in is not on this device: enter it again".to_owned()
            })?;
            match ask.auth_scheme.as_str() {
                "bearer" => headers.push(("Authorization".into(), format!("Bearer {secret}"))),
                "basic" => {
                    let raw = format!("{}:{secret}", sub(&ask.auth_username));
                    let encoded = base64::engine::general_purpose::STANDARD.encode(raw.as_bytes());
                    headers.push(("Authorization".into(), format!("Basic {encoded}")));
                }
                _ => {
                    let name = sub(&ask.auth_header);
                    if !name.trim().is_empty() {
                        headers.push((name, secret));
                    }
                }
            }
        }
        _ => {}
    }

    let body = match ask.body_mode {
        BodyMode::None => None,
        BodyMode::Json => {
            if !has_content_type(&headers) {
                headers.push(("Content-Type".into(), "application/json".into()));
            }
            Some(sub(&ask.body))
        }
        BodyMode::Raw => Some(sub(&ask.body)),
        BodyMode::Form => {
            if !has_content_type(&headers) {
                headers.push((
                    "Content-Type".into(),
                    "application/x-www-form-urlencoded".into(),
                ));
            }
            let pairs: Vec<String> = ask
                .form
                .iter()
                .filter(|(key, _)| !sub(key).trim().is_empty())
                .map(|(key, value)| {
                    format!(
                        "{}={}",
                        encode_component(&sub(key)),
                        encode_component(&sub(value))
                    )
                })
                .collect();
            Some(pairs.join("&"))
        }
    };

    Ok(Outgoing {
        method: ask.method.to_ascii_uppercase(),
        url,
        headers,
        body,
    })
}

/// How a job is doing, as the app reads it.
struct Job {
    cancel: Arc<AtomicBool>,
    answer: Mutex<Option<Result<serde_json::Value, String>>>,
    cancelled: AtomicBool,
}

fn jobs() -> &'static Mutex<std::collections::BTreeMap<String, Arc<Job>>> {
    static JOBS: OnceLock<Mutex<std::collections::BTreeMap<String, Arc<Job>>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(std::collections::BTreeMap::new()))
}

fn job_counter() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        });
    // The moment and the count together: unique on a device across restarts.
    let count = NEXT.fetch_add(1, Ordering::Relaxed);
    at.saturating_mul(1000).saturating_add(count)
}

fn fresh(n: u64) -> String {
    format!("api-{n}")
}

/// One `engine.call`: what is answered now. `keep_credential` keeps a value this device alone
/// holds; `forget_credential` takes one away again.
///
/// # Errors
///
/// The op is unknown, the input does not read, or the device's store refused.
pub fn call(app: &str, op: &str, input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Keep {
        value: String,
    }
    #[derive(Deserialize)]
    struct Forget {
        reference: String,
    }
    match op {
        "keep_credential" => {
            let ask: Keep = serde_json::from_str(input)
                .map_err(|error| format!("the credential does not read: {error}"))?;
            let reference = keep_credential(app, &ask.value)?;
            Ok(serde_json::json!({ "reference": reference }).to_string())
        }
        "forget_credential" => {
            let ask: Forget = serde_json::from_str(input)
                .map_err(|error| format!("the reference does not read: {error}"))?;
            forget_credential(app, &ask.reference)?;
            Ok("{}".to_owned())
        }
        other => Err(format!("`{other}` is no op this device runs for {app}")),
    }
}

/// One `engine.start`: a request on its way, answered with the job's id.
///
/// # Errors
///
/// The op is unknown, the request does not read, or the device could not start it.
pub fn start(app: &str, op: &str, input: &str) -> Result<String, String> {
    if op != "send" {
        return Err(format!("`{op}` is no op this device runs for {app}"));
    }
    let ask: Ask = serde_json::from_str(input)
        .map_err(|error| format!("the request does not read: {error}"))?;
    let outgoing = build_outgoing(app, &ask)?;
    let job = Arc::new(Job {
        cancel: Arc::new(AtomicBool::new(false)),
        answer: Mutex::new(None),
        cancelled: AtomicBool::new(false),
    });
    let id = fresh(job_counter());
    jobs()
        .lock()
        .map_err(|_| "the job table is unreadable".to_owned())?
        .insert(id.clone(), Arc::clone(&job));
    let worker = Arc::clone(&job);
    std::thread::Builder::new()
        .name(format!("api-client {id}"))
        .spawn(move || {
            let answer = send(&outgoing, &worker.cancel);
            if worker.cancel.load(Ordering::Acquire) {
                worker.cancelled.store(true, Ordering::Release);
            }
            if let Ok(mut slot) = worker.answer.lock() {
                *slot = Some(answer);
            }
        })
        .map_err(|error| format!("this device could not start the request: {error}"))?;
    Ok(id)
}

/// How far the job `job` has got, as JSON: its state and what it answered.
///
/// # Errors
///
/// No such job on this device.
pub fn status(job: &str) -> Result<String, String> {
    let found = jobs()
        .lock()
        .map_err(|_| "the job table is unreadable".to_owned())?
        .get(job)
        .cloned();
    let Some(job) = found else {
        return Err(format!("no job `{job}` runs on this device"));
    };
    if job.cancelled.load(Ordering::Acquire) {
        return Ok(serde_json::json!({
            "state": "cancelled",
            "answer": "",
            "error": "The request was cancelled.",
        })
        .to_string());
    }
    let answer = job
        .answer
        .lock()
        .map_err(|_| "the job is unreadable".to_owned())?;
    match answer.as_ref() {
        None => Ok(serde_json::json!({
            "state": "running",
            "answer": "",
            "error": "",
        })
        .to_string()),
        Some(Ok(value)) => Ok(serde_json::json!({
            "state": "done",
            "answer": value.to_string(),
            "error": "",
        })
        .to_string()),
        Some(Err(error)) => Ok(serde_json::json!({
            "state": "failed",
            "answer": "",
            "error": error,
        })
        .to_string()),
    }
}

/// Stop the job `job`: false when it is not this device's or it had finished.
pub fn cancel(job: &str) -> bool {
    let Ok(known) = jobs().lock() else {
        return false;
    };
    let Some(state) = known.get(job) else {
        return false;
    };
    if state.answer.lock().is_ok_and(|answer| answer.is_some()) {
        return false;
    }
    state.cancel.store(true, Ordering::Release);
    true
}

fn client() -> Result<&'static tilo_net::http::BlockingClient, String> {
    static CLIENT: OnceLock<Result<tilo_net::http::BlockingClient, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            tilo_net::http::blocking_client()
                .user_agent("tilo-api-tester/0.1")
                .timeout(REQUEST_TIMEOUT)
                .build()
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Perform one request and collect the response, stopping when `cancelled` is set.
fn send(out: &Outgoing, cancelled: &AtomicBool) -> Result<serde_json::Value, String> {
    let client = client()?;
    let method = tilo_net::Method::from_bytes(out.method.as_bytes())
        .map_err(|error| format!("invalid method: {error}"))?;
    let mut builder = client.request(method, &out.url);
    for (name, value) in &out.headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    if let Some(body) = &out.body {
        builder = builder.body(body.clone());
    }

    let started = std::time::Instant::now();
    let mut resp = builder.send().map_err(|error| {
        if error.is_timeout() {
            format!("No answer within {} seconds.", REQUEST_TIMEOUT.as_secs())
        } else if error.is_connect() {
            format!("Could not reach the server: {error}")
        } else {
            error.to_string()
        }
    })?;
    let status = resp.status();
    let status_text = status.canonical_reason().unwrap_or("").to_string();
    let headers: Vec<(String, String)> = resp
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                value.to_str().unwrap_or("").to_owned(),
            )
        })
        .collect();
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let declared = resp.content_length();
    let (bytes, complete) = read_shown(&mut resp, cancelled)?;
    if cancelled.load(Ordering::Acquire) {
        return Err("The request was cancelled.".to_owned());
    }
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let received = bytes.len();
    let size_bytes = match (complete, declared.and_then(|n| usize::try_from(n).ok())) {
        (true, _) | (false, None) => received,
        (false, Some(declared)) => declared.max(received),
    };
    let shown = ShownBody::of(bytes, complete, textual(&content_type));

    Ok(serde_json::json!({
        "status": status.as_u16(),
        "status_text": status_text,
        "duration_ms": duration_ms,
        "size_bytes": size_bytes,
        "size_exact": complete || declared.is_some(),
        "shown_bytes": shown.bytes,
        "binary": shown.binary,
        "headers": headers,
        "body": shown.text,
        "content_type": content_type,
    }))
}

/// `bytes` as a `Read`, keeping the first [`MAX_SHOWN_BODY_BYTES`] and saying whether that was
/// all of it. A cancel stops the read.
fn read_shown(resp: &mut impl Read, cancelled: &AtomicBool) -> Result<(Vec<u8>, bool), String> {
    let mut body = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Ok((body, false));
        }
        let read = resp
            .read(&mut chunk)
            .map_err(|error| format!("the reply stopped: {error}"))?;
        if read == 0 {
            return Ok((body, true));
        }
        let room = MAX_SHOWN_BODY_BYTES.saturating_sub(body.len());
        if read > room {
            body.extend_from_slice(chunk.get(..room).unwrap_or_default());
            return Ok((body, false));
        }
        body.extend_from_slice(chunk.get(..read).unwrap_or_default());
    }
}

struct ShownBody {
    text: String,
    /// How many of the body's bytes `text` shows.
    bytes: usize,
    binary: bool,
}

impl ShownBody {
    /// Text as text; anything else as a hex dump of its start. A body cut short may end inside
    /// a character, which is not binary.
    fn of(bytes: Vec<u8>, complete: bool, textual: bool) -> Self {
        match String::from_utf8(bytes) {
            Ok(text) => Self {
                bytes: text.len(),
                text,
                binary: false,
            },
            Err(error) => {
                let utf8 = error.utf8_error();
                let bytes = error.into_bytes();
                if textual || (!complete && utf8.error_len().is_none()) {
                    let end = if textual {
                        bytes.len()
                    } else {
                        utf8.valid_up_to()
                    };
                    let shown = bytes.get(..end).unwrap_or_default();
                    return Self {
                        bytes: shown.len(),
                        text: String::from_utf8_lossy(shown).into_owned(),
                        binary: false,
                    };
                }
                let dumped = bytes.get(..MAX_HEX_BYTES).unwrap_or(&bytes).to_vec();
                Self {
                    text: hex_dump(&dumped),
                    bytes: dumped.len(),
                    binary: true,
                }
            }
        }
    }
}

/// Whether `content_type` names text, which is shown as text even when it is not UTF-8.
fn textual(content_type: &str) -> bool {
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    essence.starts_with("text/")
        || ["json", "xml", "javascript", "x-www-form-urlencoded"]
            .iter()
            .any(|kind| essence.contains(kind))
}

/// `bytes` as lines of sixteen: the offset, the hex, and the printable ASCII.
fn hex_dump(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len().div_ceil(16).saturating_mul(78));
    for (row, line) in bytes.chunks(16).enumerate() {
        let _ = write!(out, "{:08x} ", row.saturating_mul(16));
        for column in 0..16 {
            match line.get(column) {
                Some(byte) => {
                    let _ = write!(out, " {byte:02x}");
                }
                None => out.push_str("   "),
            }
        }
        out.push_str("  ");
        out.extend(line.iter().map(|&byte| {
            if byte.is_ascii_graphic() || byte == b' ' {
                char::from(byte)
            } else {
                '.'
            }
        }));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    /// The app the request tests act for.
    const APP: &str = "so.tilo.api-tester";
    use super::*;

    fn ask(method: &str, url: &str) -> Ask {
        Ask {
            method: method.into(),
            url: url.into(),
            params: Vec::new(),
            headers: Vec::new(),
            vars: Vec::new(),
            auth_scheme: String::new(),
            auth_username: String::new(),
            auth_header: String::new(),
            auth_reference: String::new(),
            body_mode: BodyMode::None,
            body: String::new(),
            form: Vec::new(),
        }
    }

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String, String)> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).into(), (*value).into(), String::new()))
            .collect()
    }

    #[test]
    fn variables_resolve_where_written_and_an_unknown_one_stays_visible() {
        let vars: std::collections::HashMap<String, String> =
            [("base", "https://api.example.com"), ("id", "7")]
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect();
        assert_eq!(
            substitute("{{base}}/users/{{ id }}", &vars),
            "https://api.example.com/users/7"
        );
        assert_eq!(substitute("{{missing}}", &vars), "{{missing}}");
        assert_eq!(substitute("no braces", &vars), "no braces");
        assert_eq!(substitute("a {{ b", &vars), "a {{ b");
    }

    #[test]
    fn query_pairs_precede_fragments_and_reuse_query_separators() {
        for (base, expected) in [
            (
                "/search#section?ignored",
                "/search?q=rust%20lang#section?ignored",
            ),
            ("/search?sort=id#top", "/search?sort=id&q=rust%20lang#top"),
            ("/search?#", "/search?q=rust%20lang#"),
            ("/search?sort=id&", "/search?sort=id&q=rust%20lang"),
        ] {
            let mut ask = ask("GET", base);
            ask.params = vec![("q".into(), "rust lang".into())];
            assert_eq!(build_outgoing(APP, &ask).unwrap().url, expected, "base: {base}");

            ask.params.clear();
            assert_eq!(build_outgoing(APP, &ask).unwrap().url, base);
        }
        for suffix in ["", "#section?ignored"] {
            let mut ask = ask("GET", &format!("https://example.com/items&{suffix}"));
            ask.params = vec![("q".into(), "test".into())];
            assert_eq!(
                build_outgoing(APP, &ask).unwrap().url,
                format!("https://example.com/items&?q=test{suffix}")
            );
        }
    }

    #[test]
    fn variables_reach_the_url_params_headers_and_body() {
        let mut ask = ask("GET", "{{base}}/search");
        ask.params = vec![("q".into(), "rust lang".into()), ("off".into(), "1".into())];
        ask.headers = vec![
            ("X-Trace".into(), "{{trace}}".into()),
            ("  ".into(), "unnamed".into()),
        ];
        ask.vars = env(&[("base", "https://api.example.com"), ("trace", "abc")]);
        let out = build_outgoing(APP, &ask).unwrap();
        assert_eq!(
            out.url,
            "https://api.example.com/search?q=rust%20lang&off=1"
        );
        assert_eq!(out.headers, vec![("X-Trace".into(), "abc".into())]);
    }

    #[test]
    fn a_body_follows_its_mode_and_keeps_a_content_type_it_was_given() {
        let mut ask = ask("post", "https://x.test");
        ask.body_mode = BodyMode::Json;
        ask.body = r#"{"id":{{id}}}"#.into();
        ask.vars = env(&[("id", "7")]);
        let out = build_outgoing(APP, &ask).unwrap();
        assert_eq!(out.method, "POST");
        assert_eq!(out.body.as_deref(), Some(r#"{"id":7}"#));
        assert_eq!(
            out.headers,
            vec![("Content-Type".into(), "application/json".into())]
        );

        ask.body_mode = BodyMode::Form;
        ask.headers = vec![("content-type".into(), "text/plain".into())];
        ask.form = vec![
            ("q".into(), "a&b=c".into()),
            ("off".into(), "1".into()),
            ("name".into(), "{{id}}".into()),
        ];
        let out = build_outgoing(APP, &ask).unwrap();
        assert_eq!(out.body.as_deref(), Some("q=a%26b%3Dc&off=1&name=7"));
        assert_eq!(
            out.headers,
            vec![("content-type".into(), "text/plain".into())]
        );

        ask.body_mode = BodyMode::None;
        ask.headers.clear();
        assert!(build_outgoing(APP, &ask).unwrap().body.is_none());
    }

    #[test]
    fn a_binary_body_is_a_hex_dump_and_a_text_body_is_shown_whole() {
        let text = ShownBody::of(b"hello".to_vec(), true, true);
        assert!(!text.binary);
        assert_eq!(text.text, "hello");

        let binary = ShownBody::of(vec![0xff, 0x00, b'A'], true, false);
        assert!(binary.binary);
        assert_eq!(
            binary.text,
            "00000000  ff 00 41                                         ..A\n"
        );
        assert_eq!(binary.bytes, 3);

        // A body cut inside a character is not binary: it shows the text before the cut.
        let cut = ShownBody::of(vec![b'o', b'k', 0xc3], false, false);
        assert!(!cut.binary);
        assert_eq!(cut.text, "ok");
        assert_eq!(cut.bytes, 2);

        assert!(textual("application/json; charset=utf-8"));
        assert!(textual("text/plain"));
        assert!(!textual("image/png"));
        assert!(!textual(""));
    }

    #[test]
    fn a_job_is_unknown_until_it_starts_and_a_bad_op_is_refused() {
        assert!(status("no-such-job").is_err());
        assert!(!cancel("no-such-job"));
        assert!(start(APP, "paint", "{}").is_err());
        assert!(start(APP, "send", "not json").is_err());
        assert!(call(APP, "paint", "{}").is_err());
    }

    #[test]
    fn a_credential_is_kept_and_forgotten_on_this_device() {
        tilo_secrets::keep_in_memory_for_process();
        let reference = keep_credential(APP, "private-token").unwrap();
        assert_eq!(credential(APP, &reference).as_deref(), Some("private-token"));
        forget_credential(APP, &reference).unwrap();
        assert_eq!(credential(APP, &reference), None);

        let mut ask = ask("GET", "https://x.test");
        ask.auth_scheme = "bearer".into();
        ask.auth_reference = keep_credential(APP, "secret123").unwrap();
        assert_eq!(
            build_outgoing(APP, &ask).unwrap().headers,
            vec![("Authorization".into(), "Bearer secret123".into())]
        );
        ask.auth_scheme = "basic".into();
        ask.auth_username = "user".into();
        assert_eq!(
            build_outgoing(APP, &ask).unwrap().headers,
            vec![("Authorization".into(), "Basic dXNlcjpzZWNyZXQxMjM=".into())]
        );
        ask.auth_scheme = "api_key".into();
        ask.auth_header = "X-Api-Key".into();
        assert_eq!(
            build_outgoing(APP, &ask).unwrap().headers,
            vec![("X-Api-Key".into(), "secret123".into())]
        );
        ask.auth_reference = "not-there".into();
        assert!(build_outgoing(APP, &ask).is_err());
        forget_credential(APP, &ask.auth_reference).ok();
    }
}
