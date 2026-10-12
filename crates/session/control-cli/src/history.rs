//! Stateless client of published native history actions. No revision index or ID codec.
use super::{ParseError, READ_ANSWER_WAIT_MS, READ_DISCOVERY_WAIT_MS, remaining_ms, settled};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    os::unix::net::UnixStream,
    time::{Duration, Instant},
};

// The compositor counts the terminating newline in its request-line limit.
const MAX_WIRE_BYTES: usize = 64 * 1024;
const MAX_REVIEW_BYTES: usize = 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Request {
    verb: String,
    flags: BTreeMap<String, String>,
    args: Value,
}

fn usage(message: impl Into<String>) -> ParseError {
    ParseError::Usage(message.into())
}

pub(super) fn parse(iter: &mut impl Iterator<Item = String>) -> Result<Request, ParseError> {
    let verb = iter
        .next()
        .ok_or_else(|| usage("history requires log/read/diff/review/restore/checkpoint/status"))?;
    if !matches!(
        verb.as_str(),
        "log" | "read" | "diff" | "review" | "restore" | "checkpoint" | "status"
    ) {
        return Err(usage(format!("unknown history command `{verb}`")));
    }
    let mut flags = BTreeMap::new();
    while let Some(flag) = iter.next() {
        if !matches!(
            flag.as_str(),
            "--application"
                | "--room"
                | "--record"
                | "--resource"
                | "--provider"
                | "--id"
                | "--args-json"
                | "--review-file"
                | "--invocation-id"
        ) {
            return Err(usage(format!("history: unknown option `{flag}`")));
        }
        let value = iter
            .next()
            .filter(|s| !s.is_empty() && !s.starts_with("--"))
            .ok_or_else(|| usage(format!("{flag} requires a value")))?;
        if flags.insert(flag.clone(), value).is_some() {
            return Err(usage(format!("duplicate {flag}")));
        }
    }
    if verb == "status" {
        if flags.len() != 1
            || flags
                .get("--invocation-id")
                .and_then(|s| s.parse::<u64>().ok())
                .is_none()
        {
            return Err(usage("history status requires only --invocation-id UINT"));
        }
        return Ok(Request {
            verb,
            flags,
            args: json!({}),
        });
    }
    if flags.contains_key("--invocation-id") {
        return Err(usage("--invocation-id is only for history status"));
    }
    let hierarchy = ["--application", "--room", "--record", "--resource"];
    let exact = ["--provider", "--id"];
    let has_hierarchy = hierarchy.iter().any(|k| flags.contains_key(*k));
    let has_exact = exact.iter().any(|k| flags.contains_key(*k));
    if has_hierarchy == has_exact
        || (has_hierarchy && !hierarchy.iter().all(|k| flags.contains_key(*k)))
        || (has_exact && !exact.iter().all(|k| flags.contains_key(*k)))
    {
        return Err(usage(
            "history requires all of --application --room --record --resource OR exact discovered --provider --id",
        ));
    }
    if flags
        .get("--args-json")
        .is_some_and(|s| s.len() >= MAX_WIRE_BYTES)
    {
        return Err(usage(
            "history arguments exceed the 64 KiB request-line limit",
        ));
    }
    let args: Value = flags
        .get("--args-json")
        .map_or(Ok(json!({})), |s| serde_json::from_str(s))
        .map_err(|e| usage(format!("invalid history arguments: {e}")))?;
    validate_args(&verb, &args)?;
    if (verb == "restore") != flags.contains_key("--review-file")
        || (verb == "restore" && flags.contains_key("--args-json"))
    {
        return Err(usage(
            "restore requires --review-file and forbids --args-json; other verbs forbid --review-file",
        ));
    }
    Ok(Request { verb, flags, args })
}

fn validate_args(verb: &str, args: &Value) -> Result<(), ParseError> {
    let object = args
        .as_object()
        .ok_or_else(|| usage("history arguments must be an object"))?;
    let (allowed, required): (&[&str], &[&str]) = match verb {
        "log" => (&["last"], &[]),
        "read" => (&["item", "version"], &[]),
        "diff" => (&["from", "to"], &["from"]),
        "review" => (&["version", "paths"], &["version", "paths"]),
        "checkpoint" => (&["name"], &["name"]),
        _ => (&[], &[]),
    };
    if object.keys().any(|k| !allowed.contains(&k.as_str()))
        || required.iter().any(|k| !object.contains_key(*k))
    {
        return Err(usage(format!(
            "{verb}: allowed arguments {allowed:?}, required {required:?}"
        )));
    }
    for (key, value) in object {
        let valid = match key.as_str() {
            "last" => value.as_u64().is_some_and(|n| (1..=500).contains(&n)),
            "paths" => valid_paths(value),
            _ => nonempty_text(value),
        };
        if !valid {
            return Err(usage(format!("invalid {verb} argument `{key}`")));
        }
    }
    Ok(())
}

fn valid_paths(value: &Value) -> bool {
    value.as_array().is_some_and(|paths| {
        (1..=256).contains(&paths.len())
            && paths.iter().all(|path| {
                nonempty_text(path) && path.as_str().is_some_and(|text| text.len() <= 1024)
            })
    })
}

fn nonempty_text(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|s| !s.trim().is_empty() && !s.chars().any(char::is_control))
}

impl Request {
    fn operation(&self) -> &str {
        match self.verb.as_str() {
            "log" => "history",
            "review" => "review-restore",
            "restore" => "restore-reviewed",
            other => other,
        }
    }
}

fn failure(code: &str, message: impl Into<String>) -> Value {
    json!({"err": {"code": code, "message": message.into()}})
}

/// Return the full wire answer even on denial, stale reviews, or terminal job failures.
fn exchange(stream: &mut UnixStream, request: &Value, start: Instant) -> Result<Value, Value> {
    let mut wire = request.to_string();
    wire.push('\n');
    if wire.len() > MAX_WIRE_BYTES {
        return Err(failure(
            "request_too_large",
            "serialized request exceeds 64 KiB",
        ));
    }
    let response = timed_round_trip(stream, wire.as_bytes(), start)
        .map_err(|e| failure("transport", e.to_string()))?;
    if response.get("err").is_some() {
        Err(response)
    } else {
        Ok(response)
    }
}

/// Each I/O syscall gets only the remaining monotonic budget, including partial
/// writes and trickling responses. Other control commands keep their existing I/O.
fn io_budget(start: Instant) -> std::io::Result<Duration> {
    Duration::from_millis(READ_ANSWER_WAIT_MS)
        .checked_sub(start.elapsed())
        .filter(|left| !left.is_zero())
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::TimedOut, "history completion deadline")
        })
}

fn timed_round_trip(
    stream: &mut UnixStream,
    mut wire: &[u8],
    start: Instant,
) -> std::io::Result<Value> {
    while !wire.is_empty() {
        stream.set_write_timeout(Some(io_budget(start)?))?;
        let written = stream.write(wire)?;
        if written == 0 {
            return Err(std::io::ErrorKind::WriteZero.into());
        }
        wire = wire.get(written..).unwrap_or_default();
    }
    let mut response = Vec::new();
    loop {
        stream.set_read_timeout(Some(io_budget(start)?))?;
        let mut chunk = [0; 8192];
        let count = stream.read(&mut chunk)?;
        if count == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        let bytes = chunk.get(..count).unwrap_or_default();
        if response.len().saturating_add(bytes.len()) > MAX_RESPONSE_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "history response exceeds 16 MiB",
            ));
        }
        response.extend_from_slice(bytes);
        if bytes.contains(&b'\n') {
            return serde_json::from_slice(&response)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e));
        }
    }
}

fn target(row: &Value) -> Result<Value, Value> {
    let mut target = serde_json::Map::new();
    for key in ["application", "room", "record", "resource"] {
        let value = row.get(key).filter(|v| nonempty_text(v)).ok_or_else(|| {
            failure(
                "incompatible_protocol",
                format!("published action lacks {key}"),
            )
        })?;
        target.insert(key.into(), value.clone());
    }
    Ok(Value::Object(target))
}

fn discover(stream: &mut UnixStream, request: &Request) -> Result<Value, Value> {
    // Query the whole catalog: labels and query scores are not identities.
    let start = Instant::now();
    let mut rows = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..1024 {
        let mut wire = json!({"op":"actions_enumerate", "query":"", "include_programmatic":true, "wait_ms":READ_DISCOVERY_WAIT_MS, "limit":500});
        if let Some(cursor) = &cursor
            && let Some(fields) = wire.as_object_mut()
        {
            fields.insert("cursor".into(), Value::String(cursor.clone()));
        }
        let response = exchange(stream, &wire, start)?;
        let answer = &response.get("answer").unwrap_or(&Value::Null);
        if answer.get("complete").unwrap_or(&Value::Null) != true {
            return Err(failure(
                "discovery_pending",
                "catalog is incomplete; retry discovery",
            ));
        }
        let page = answer
            .get("actions")
            .unwrap_or(&Value::Null)
            .as_array()
            .ok_or_else(|| failure("incompatible_protocol", "missing action catalog"))?;
        rows.extend(
            page.iter()
                .filter(|row| {
                    row.get("provider").unwrap_or(&Value::Null) == "app-actions"
                        && row.get("operation").unwrap_or(&Value::Null) == request.operation()
                        && if let Some(provider) = request.flags.get("--provider") {
                            row.get("provider").unwrap_or(&Value::Null) == provider.as_str()
                                && row.get("id").unwrap_or(&Value::Null)
                                    == request
                                        .flags
                                        .get("--id")
                                        .map(String::as_str)
                                        .unwrap_or_default()
                        } else {
                            ["application", "room", "record", "resource"]
                                .iter()
                                .all(|key| {
                                    row.get(*key).unwrap_or(&Value::Null)
                                        == request
                                            .flags
                                            .get(&format!("--{key}"))
                                            .map(String::as_str)
                                            .unwrap_or_default()
                                })
                        }
                })
                .cloned(),
        );
        cursor = answer
            .get("next_cursor")
            .unwrap_or(&Value::Null)
            .as_str()
            .map(str::to_owned);
        if cursor.is_none() {
            if answer.get("truncated").unwrap_or(&Value::Null) == true {
                return Err(failure(
                    "discovery_pending",
                    "truncated catalog without a next cursor",
                ));
            }
            return match rows.len() {
                1 => Ok(rows.remove(0)),
                0 => Err(failure(
                    "missing_target",
                    "no exact published native action matches the target and operation",
                )),
                _ => Err(failure(
                    "ambiguous_target",
                    "multiple published actions match; use an exact discovered provider/id",
                )),
            };
        }
    }
    Err(failure(
        "discovery_pending",
        "catalog exceeded the page budget",
    ))
}

fn follow(stream: &mut UnixStream, mut response: Value, start: Instant) -> Result<Value, Value> {
    for _ in 0..64 {
        let update = &response
            .get("answer")
            .unwrap_or(&Value::Null)
            .get("receipt")
            .unwrap_or(&Value::Null)
            .get("update")
            .unwrap_or(&Value::Null);
        let state = update
            .get("state")
            .unwrap_or(&Value::Null)
            .as_str()
            .ok_or_else(|| failure("incompatible_protocol", "missing invocation state"))?;
        if settled(state) {
            return Ok(response);
        }
        let job = response
            .get("answer")
            .unwrap_or(&Value::Null)
            .get("receipt")
            .unwrap_or(&Value::Null)
            .get("broker_invocation_id")
            .unwrap_or(&Value::Null)
            .as_u64()
            .ok_or_else(|| failure("incompatible_protocol", "missing job id"))?;
        let wait = remaining_ms(READ_ANSWER_WAIT_MS, start);
        if wait == 0 {
            return Ok(response);
        }
        response = exchange(
            stream,
            &json!({"op":"actions_status", "invocation_id":job, "wait_ms":wait}),
            start,
        )?;
    }
    Ok(response)
}

fn validate_review(review: &Value) -> Result<(), Value> {
    let valid = review.as_object().is_some_and(|object| {
        object.len() == 4
            && review.get("schema").unwrap_or(&Value::Null) == 1
            && ["target_heads", "live_heads"].iter().all(|key| {
                review
                    .get(*key)
                    .unwrap_or(&Value::Null)
                    .as_array()
                    .is_some_and(|heads| {
                        !heads.is_empty()
                            && heads.len() <= 64
                            && heads.iter().all(|head| {
                                head.as_str().is_some_and(|hash| {
                                    hash.len() == 64
                                        && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                                })
                            })
                            && heads.iter().enumerate().all(|(index, head)| {
                                !heads.iter().take(index).any(|earlier| {
                                    earlier
                                        .as_str()
                                        .zip(head.as_str())
                                        .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
                                })
                            })
                    })
            })
            && valid_paths(review.get("paths").unwrap_or(&Value::Null))
    });
    if valid {
        Ok(())
    } else {
        Err(failure(
            "review_file",
            "invalid native review: require schema 1, exact paths and unique full causal hashes",
        ))
    }
}

fn execute(stream: &mut UnixStream, request: &Request) -> Result<Value, Value> {
    if request.verb == "status" {
        let job = request
            .flags
            .get("--invocation-id")
            .map(String::as_str)
            .unwrap_or_default()
            .parse::<u64>()
            .map_err(|e| failure("invalid_arguments", e.to_string()))?;
        let start = Instant::now();
        let response = exchange(
            stream,
            &json!({"op":"actions_status", "invocation_id":job, "wait_ms":remaining_ms(READ_ANSWER_WAIT_MS, start)}),
            start,
        )?;
        return follow(stream, response, start);
    }
    let row = discover(stream, request)?;
    let binding = target(&row)?;
    let mut args = request.args.clone();
    if request.verb == "restore" {
        let mut bytes = Vec::new();
        std::fs::File::open(
            request
                .flags
                .get("--review-file")
                .map(String::as_str)
                .unwrap_or_default(),
        )
        .and_then(|file| {
            file.take((MAX_REVIEW_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
        })
        .map_err(|e| failure("review_file", e.to_string()))?;
        if bytes.len() > MAX_REVIEW_BYTES {
            return Err(failure("review_file", "review file exceeds 1 MiB"));
        }
        let file: Value =
            serde_json::from_slice(&bytes).map_err(|e| failure("review_file", e.to_string()))?;
        if file.as_object().is_none_or(|object| {
            object.len() != 4
                || !["target", "operation", "result", "response"]
                    .iter()
                    .all(|key| object.contains_key(*key))
        }) || file.get("operation").unwrap_or(&Value::Null) != "review-restore"
            || file.get("result").unwrap_or(&Value::Null)
                != file
                    .get("response")
                    .unwrap_or(&Value::Null)
                    .get("answer")
                    .unwrap_or(&Value::Null)
                    .get("receipt")
                    .unwrap_or(&Value::Null)
                    .get("update")
                    .unwrap_or(&Value::Null)
                    .get("output")
                    .unwrap_or(&Value::Null)
                    .get("value")
                    .unwrap_or(&Value::Null)
            || file.get("target").unwrap_or(&Value::Null) != &binding
            || file
                .get("response")
                .unwrap_or(&Value::Null)
                .get("answer")
                .unwrap_or(&Value::Null)
                .get("receipt")
                .unwrap_or(&Value::Null)
                .get("update")
                .unwrap_or(&Value::Null)
                .get("state")
                .unwrap_or(&Value::Null)
                != "succeeded"
        {
            return Err(failure(
                "review_target_mismatch",
                "review must be a successful history review for this exact target",
            ));
        }
        let review = &file
            .get("result")
            .unwrap_or(&Value::Null)
            .get("review")
            .unwrap_or(&Value::Null);
        validate_review(review)?;
        args = json!({"review":review});
    }
    let start = Instant::now();
    let initial = exchange(
        stream,
        &json!({"op":"actions_invoke", "provider":row.get("provider").unwrap_or(&Value::Null), "id":row.get("id").unwrap_or(&Value::Null), "args":args, "wait_ms":remaining_ms(READ_ANSWER_WAIT_MS, start)}),
        start,
    )?;
    let response = follow(stream, initial, start)?;
    Ok(
        json!({"target":binding, "operation":request.operation(), "result":response.pointer("/answer/receipt/update/output/value"), "response":response}),
    )
}

pub(super) fn run(stream: &mut UnixStream, request: &Request) -> u8 {
    let value = execute(stream, request).unwrap_or_else(|error| error);
    let response = value.get("response").unwrap_or(&value);
    let state = response
        .pointer("/answer/receipt/update/state")
        .and_then(Value::as_str);
    let code = if matches!(
        response.pointer("/err/code").and_then(Value::as_str),
        Some("transport" | "request_too_large")
    ) {
        2
    } else if response.get("err").is_some() || matches!(state, Some("failed" | "cancelled")) {
        1
    } else if state == Some("succeeded") {
        0
    } else {
        3
    };
    println!("{value}");
    code
}
