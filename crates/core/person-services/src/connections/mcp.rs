//! A connected service, asked as the Model Context Protocol asks one over
//! streamable HTTP: one JSON-RPC request posted per question, answered as
//! JSON or as an event stream that carries the answer.
//!
//! What the service answers is data from outside. This reads what it needs
//! of it (the protocol's version, the name the service gives itself, its
//! commands, an answer to a call), each bounded, and nothing a service
//! sends is ever acted on: a request a service sends back down its stream
//! is not answered.

use std::ops::ControlFlow;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Map, Value, json};
use tilo_lock::MutexRecover as _;
use tilo_net::{HeaderValue, Url};
use wire_frame::sse;

use super::address::Address;
use super::said::{self, Offered};
use super::wire::{Ask, Problem, Reach, Sent, Whole, Wire};

/// The versions of the protocol this server speaks, newest first.
const PROTOCOLS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
/// How long a service may take to say what it is and to list its commands.
const ASK_WAIT: Duration = Duration::from_secs(20);
/// How long a service may take to answer a call of one of its commands.
const CALL_WAIT: Duration = Duration::from_secs(60);
/// The most of one answer that is read.
const MAX_ANSWER_BYTES: usize = 2 * 1024 * 1024;
/// The most pages of commands read from one service.
pub(super) const MAX_PAGES: usize = 16;
/// The most commands of one service that are offered.
pub(crate) const MAX_COMMANDS: usize = 256;
/// The most a service's commands weigh together as they are kept: their
/// words and the descriptions of their arguments. A person's server reads
/// and answers a connection whole, so one service cannot make that large.
const MAX_COMMANDS_BYTES: usize = 1024 * 1024;
/// The longest session id taken from a service.
const MAX_SESSION_BYTES: usize = 256;

/// What a service said it is when it was greeted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Told {
    /// The name it gives itself: one bounded line of its own words.
    pub(crate) name: String,
}

/// The commands a service listed.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Listed {
    pub(crate) offered: Vec<Offered>,
    /// How many more it listed that are not offered.
    pub(crate) unusable: u32,
    /// What the offered ones weigh together.
    weight: usize,
}

/// What `command` weighs as kept.
fn weight(command: &Offered) -> usize {
    [
        command.name.len(),
        command.title.as_ref().map_or(0, String::len),
        command.description.len(),
        command.schema.to_string().len(),
    ]
    .into_iter()
    .fold(0, usize::saturating_add)
}

impl Listed {
    /// Take one more command the service listed: it is offered when it is
    /// one this server offers at all, is not listed a second time, and fits
    /// beside the others; otherwise it is counted and left out.
    fn take(&mut self, command: Option<Offered>) {
        let weighs = command.as_ref().map_or(0, weight);
        let fits = command.as_ref().is_some_and(|command| {
            self.offered.len() < MAX_COMMANDS
                && self.offered.iter().all(|kept| kept.name != command.name)
                && self.weight.saturating_add(weighs) <= MAX_COMMANDS_BYTES
        });
        match command {
            Some(command) if fits => {
                self.weight = self.weight.saturating_add(weighs);
                self.offered.push(command);
            }
            _ => self.unusable = self.unusable.saturating_add(1),
        }
    }
}

/// What a service answered a call. All of it is data from outside.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Answered {
    pub(crate) content: Vec<Value>,
    pub(crate) structured: Option<Value>,
    /// Whether the service says the command failed.
    pub(crate) is_error: bool,
}

/// One conversation with a service, from its greeting on.
pub(crate) struct Session {
    wire: Wire,
    url: Url,
    bearer: Option<HeaderValue>,
    /// What the service named this conversation, named back with every
    /// question.
    named: Option<HeaderValue>,
    protocol: Option<&'static str>,
    next: u64,
}

impl Session {
    /// Greet the service at `address`, signing in with `bearer` when the
    /// connection has a sign-in.
    pub(crate) async fn open(
        reach: &Reach,
        address: &Address,
        bearer: Option<&str>,
    ) -> Result<(Self, Told), Problem> {
        let bearer = match bearer {
            None => None,
            Some(token) => {
                let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
                    .map_err(|_| Problem::SignIn { challenge: None })?;
                value.set_sensitive(true);
                Some(value)
            }
        };
        let mut session = Self {
            wire: Wire::new(reach)?,
            url: address.url().clone(),
            bearer,
            named: None,
            protocol: None,
            next: 0,
        };
        let greeting = json!({
            "protocolVersion": PROTOCOLS.first().copied().unwrap_or_default(),
            "capabilities": {},
            "clientInfo": {"name": "Tilo", "version": env!("CARGO_PKG_VERSION")},
        });
        let (answer, sent) = session.ask("initialize", greeting, ASK_WAIT).await?;
        let spoken = answer.get("protocolVersion").and_then(Value::as_str);
        session.protocol = Some(
            PROTOCOLS
                .iter()
                .copied()
                .find(|known| Some(*known) == spoken)
                .ok_or(Problem::Unreadable)?,
        );
        // A session the service names and this server cannot name back to
        // it is an answer that does not read.
        session.named = sent
            .header("mcp-session-id")
            .filter(|id| !id.is_empty())
            .map(|id| {
                if id.len() > MAX_SESSION_BYTES {
                    return Err(Problem::Unreadable);
                }
                HeaderValue::from_str(id).map_err(|_| Problem::Unreadable)
            })
            .transpose()?;
        let about = answer.get("serverInfo");
        let name = about
            .and_then(|about| about.get("title").or_else(|| about.get("name")))
            .and_then(Value::as_str)
            .map(|name| said::line(name, said::MAX_NAME_CHARS))
            .unwrap_or_default();
        session.tell("notifications/initialized").await?;
        Ok((session, Told { name }))
    }

    /// The commands the service lists, each as this server offers it.
    pub(crate) async fn commands(&mut self) -> Result<Listed, Problem> {
        let mut listed = Listed::default();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let params = cursor
                .take()
                .map_or_else(|| json!({}), |cursor| json!({"cursor": cursor}));
            let (page, _) = self.ask("tools/list", params, ASK_WAIT).await?;
            let tools = page
                .get("tools")
                .and_then(Value::as_array)
                .ok_or(Problem::Unreadable)?;
            for tool in tools {
                listed.take(said::command(tool));
            }
            cursor = page
                .get("nextCursor")
                .and_then(Value::as_str)
                .filter(|cursor| !cursor.is_empty())
                .map(str::to_owned);
            if cursor.is_none() {
                return Ok(listed);
            }
        }
        // A service whose list does not end is read this far, and what it
        // would have listed further is not offered.
        listed.unusable = listed.unusable.saturating_add(1);
        Ok(listed)
    }

    /// Call the command `command` with `arguments`, exactly as given.
    pub(crate) async fn call(
        &mut self,
        command: &str,
        arguments: Map<String, Value>,
    ) -> Result<Answered, Problem> {
        let params = json!({"name": command, "arguments": arguments});
        let (mut answer, _) = self.ask("tools/call", params, CALL_WAIT).await?;
        let content = match answer.remove("content") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(parts)) => parts,
            Some(_) => return Err(Problem::Unreadable),
        };
        Ok(Answered {
            content,
            structured: answer
                .remove("structuredContent")
                .filter(|structured| !structured.is_null()),
            is_error: answer.get("isError").and_then(Value::as_bool) == Some(true),
        })
    }

    fn headers(&self) -> Vec<(&'static str, HeaderValue)> {
        let mut headers = vec![
            ("content-type", HeaderValue::from_static("application/json")),
            (
                "accept",
                HeaderValue::from_static("application/json, text/event-stream"),
            ),
        ];
        if let Some(protocol) = self.protocol {
            headers.push(("mcp-protocol-version", HeaderValue::from_static(protocol)));
        }
        if let Some(named) = &self.named {
            headers.push(("mcp-session-id", named.clone()));
        }
        if let Some(bearer) = &self.bearer {
            headers.push(("authorization", bearer.clone()));
        }
        headers
    }

    /// Ask `method` with `params` and read the answer to exactly that
    /// question.
    async fn ask(
        &mut self,
        method: &str,
        params: Value,
        wait: Duration,
    ) -> Result<(Map<String, Value>, Sent), Problem> {
        self.next = self.next.saturating_add(1);
        let id = self.next;
        let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let stream = Arc::new(Mutex::new(StreamAnswer::new(id)));
        let reading = Arc::clone(&stream);
        let whole: Whole = Box::new(move |kind: &str, chunk: &[u8]| {
            kind.starts_with("text/event-stream") && reading.lock_recover().push(chunk)
        });
        let sent = self
            .wire
            .send(Ask {
                url: self.url.clone(),
                post: Some(serde_json::to_vec(&body).map_err(|_| Problem::Unreadable)?),
                headers: self.headers(),
                wait,
                most: MAX_ANSWER_BYTES,
                whole: Some(whole),
            })
            .await?;
        match sent.status {
            200 => {}
            401 => {
                return Err(Problem::SignIn {
                    challenge: sent.header("www-authenticate").map(str::to_owned),
                });
            }
            _ => return Err(Problem::TurnedAway),
        }
        let kind = sent.header("content-type").unwrap_or_default();
        let answer = if kind.starts_with("text/event-stream") {
            stream.lock_recover().finish()
        } else if kind.starts_with("application/json") {
            let message =
                serde_json::from_slice::<Value>(&sent.body).map_err(|_| Problem::Unreadable)?;
            answer_to(message, id)
        } else {
            None
        };
        match answer.ok_or(Problem::Unreadable)? {
            Ok(result) => Ok((result, sent)),
            Err(()) => Err(Problem::Said),
        }
    }

    /// Tell the service `method` happened; no answer is read.
    async fn tell(&self, method: &str) -> Result<(), Problem> {
        let body = json!({"jsonrpc": "2.0", "method": method});
        let sent = self
            .wire
            .send(Ask {
                url: self.url.clone(),
                post: Some(serde_json::to_vec(&body).map_err(|_| Problem::Unreadable)?),
                headers: self.headers(),
                wait: ASK_WAIT,
                most: MAX_ANSWER_BYTES,
                whole: Some(Box::new(|_: &str, _: &[u8]| true)),
            })
            .await?;
        match sent.status {
            200..=299 => Ok(()),
            401 => Err(Problem::SignIn {
                challenge: sent.header("www-authenticate").map(str::to_owned),
            }),
            _ => Err(Problem::TurnedAway),
        }
    }
}

/// What `message` answers the question `id`, when it is the answer to it:
/// its result, or that the service said it failed.
fn answer_to(message: Value, id: u64) -> Option<Result<Map<String, Value>, ()>> {
    let Value::Object(mut message) = message else {
        return None;
    };
    if message.get("id").and_then(Value::as_u64) != Some(id) {
        return None;
    }
    if message.contains_key("error") {
        return Some(Err(()));
    }
    match message.remove("result") {
        Some(Value::Object(result)) => Some(Ok(result)),
        _ => None,
    }
}

/// Finds the JSON-RPC answer to question `id` in an event stream, one arriving
/// piece at a time. While the stream is open, only events ended by a blank line
/// count; at EOF, [`Self::finish`] also checks a trailing unterminated event.
struct StreamAnswer {
    id: u64,
    sse: sse::Reader,
    found: Option<Result<Map<String, Value>, ()>>,
}

impl StreamAnswer {
    const fn new(id: u64) -> Self {
        Self {
            id,
            // Lenient UTF-8; the whole HTTP response is bounded by
            // `MAX_ANSWER_BYTES` in `wire.rs`, with no tighter per-event cap.
            sse: sse::Reader::new(sse::Utf8::Lenient, None),
            found: None,
        }
    }

    fn push(&mut self, piece: &[u8]) -> bool {
        if self.found.is_some() {
            return true;
        }
        let id = self.id;
        let found = &mut self.found;
        let _ = self.sse.push(piece, |event| {
            *found = event_answer(event.data, id);
            if found.is_some() {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        });
        self.found.is_some()
    }

    fn finish(&mut self) -> Option<Result<Map<String, Value>, ()>> {
        if self.found.is_none() {
            let id = self.id;
            let found = &mut self.found;
            let _ = self.sse.finish_unterminated(|event| {
                *found = event_answer(event.data, id);
                if found.is_some() {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            });
        }
        self.found.take()
    }
}

fn event_answer(data: &str, id: u64) -> Option<Result<Map<String, Value>, ()>> {
    if data.is_empty() {
        return None;
    }
    let message = serde_json::from_str::<Value>(data).ok()?;
    answer_to(message, id)
}

/// The answer to the question `id` in what was read of an event stream.
/// Until the stream `ended`, only an event that is whole counts.
#[cfg(test)]
fn answered(stream: &[u8], id: u64, ended: bool) -> Option<Result<Map<String, Value>, ()>> {
    let mut answer = StreamAnswer::new(id);
    answer.push(stream);
    if ended { answer.finish() } else { answer.found }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a service lists is offered up to a bound on how many commands
    /// there are and on what they weigh together, a command listed twice is
    /// offered once, and whatever is left out is counted, never cut to fit.
    #[test]
    fn a_services_commands_are_offered_up_to_what_a_connection_keeps() {
        let wordy = |name: &str| {
            said::command(&json!({
                "name": name,
                "description": "d".repeat(said::MAX_DESCRIPTION_BYTES),
            }))
        };
        let mut listed = Listed::default();
        for index in 0..100 {
            listed.take(wordy(&format!("wordy_{index}")));
        }
        let kept = listed.offered.len();
        assert!((1..100).contains(&kept), "{kept} of 100 were offered");
        assert!(listed.weight <= MAX_COMMANDS_BYTES);
        assert_eq!(
            usize::try_from(listed.unusable).unwrap(),
            100_usize.saturating_sub(kept)
        );
        assert!(
            listed
                .offered
                .iter()
                .all(|command| command.description.len() == said::MAX_DESCRIPTION_BYTES),
            "a command that is offered is offered whole"
        );

        let mut listed = Listed::default();
        for name in ["find", "plant", "find"] {
            listed.take(said::command(&json!({"name": name})));
        }
        listed.take(said::command(&json!({"name": "no plain name"})));
        let names: Vec<&str> = listed
            .offered
            .iter()
            .map(|command| command.name.as_str())
            .collect();
        assert_eq!(names, ["find", "plant"]);
        assert_eq!(listed.unusable, 2);

        let mut listed = Listed::default();
        for index in 0..300 {
            listed.take(said::command(&json!({"name": format!("small_{index}")})));
        }
        assert_eq!(listed.offered.len(), MAX_COMMANDS);
        assert_eq!(listed.unusable, 44);
    }

    /// An answer is found in a stream among what else a service sends, by
    /// the question's own id, and only once its event is whole.
    #[test]
    fn an_answer_is_read_from_a_stream_by_its_questions_id() {
        let stream = concat!(
            ": a comment\n\n",
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"sampling/createMessage\"}\n\n",
            "id: 3\r\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\r\ndata:  \"result\":{\"tools\":[]}}\r\n\r\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":9,\"result\":{\"late\":true}}",
        );
        let found = answered(stream.as_bytes(), 2, false).unwrap().unwrap();
        assert_eq!(Value::Object(found), json!({"tools": []}));
        // Another question's answer is not this one's, and a request the
        // service sends down the stream is no answer at all.
        assert!(answered(stream.as_bytes(), 3, true).is_none());
        assert!(answered(stream.as_bytes(), 7, true).is_none());
        // The last event is still arriving until the stream ended.
        assert!(answered(stream.as_bytes(), 9, false).is_none());
        assert!(answered(stream.as_bytes(), 9, true).is_some());
        let failed = "data: {\"jsonrpc\":\"2.0\",\"id\":4,\"error\":{\"code\":-32601,\"message\":\"no\"}}\n\n";
        assert_eq!(answered(failed.as_bytes(), 4, false), Some(Err(())));
        // A result that is no object is no answer this server reads.
        assert!(answer_to(json!({"jsonrpc": "2.0", "id": 1, "result": "done"}), 1).is_none());
        assert!(answer_to(json!([{"jsonrpc": "2.0", "id": 1, "result": {}}]), 1).is_none());
    }

    /// An unterminated last event (without a closing blank line) is withheld
    /// while the stream is still arriving and given out with its exact value
    /// once the stream has ended, whatever line endings it used.
    #[test]
    fn an_unterminated_last_event_is_emitted_only_at_stream_end() {
        for (stream, expected) in [
            (
                &b"data: {\"jsonrpc\":\"2.0\",\"id\":9,\"result\":{\"late\":true,\"n\":42}}"[..],
                json!({"late": true, "n": 42}),
            ),
            (
                &b"data: {\"jsonrpc\":\"2.0\",\"id\":9,\"result\":{\"tail\":\"lf\"}}\n"[..],
                json!({"tail": "lf"}),
            ),
            (
                &b"data: {\"jsonrpc\":\"2.0\",\"id\":9,\r\ndata: \"result\":{\"tail\":\"crlf\"}}\r\n"[..],
                json!({"tail": "crlf"}),
            ),
            (
                &b"data: {\"jsonrpc\":\"2.0\",\"id\":9,\"result\":{\"tail\":\"cr\"}}\r"[..],
                json!({"tail": "cr"}),
            ),
        ] {
            assert!(
                answered(stream, 9, false).is_none(),
                "must stay pending until end: {:?}",
                String::from_utf8_lossy(stream)
            );
            let found = answered(stream, 9, true).unwrap().unwrap();
            assert_eq!(Value::Object(found), expected);
            // Same result when pieces arrive one byte at a time across line ends.
            let mut incremental = StreamAnswer::new(9);
            for byte in stream {
                assert!(!incremental.push(std::slice::from_ref(byte)));
            }
            assert_eq!(Value::Object(incremental.finish().unwrap().unwrap()), expected);
        }
    }
}
