//! Live activity updates from a managed agent to its host.
//!
//! While an enrolled task runs, the agent reports what it is doing — thinking,
//! looking something up, writing, replying — as ordinary enrolled progress
//! reports whose message is [`LIVE_PREFIX`] followed by one compact JSON
//! [`AgentLiveUpdate`]. They carry no authority: the host only displays them.
//!
//! The enrolled progress carrier accepts a message of at most 8000 characters
//! and 32000 bytes (`tilo-action-broker` `enrolled_dispatch::validate_update`).
//! Every encoded update stays under [`MAX_MESSAGE_CHARS`] and
//! [`MAX_MESSAGE_BYTES`]: the reply is capped to its last 4 KiB and the
//! preview to its last 512 bytes, and when JSON escaping would still overflow
//! the bound the reply tail (then the preview tail) is shortened further.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use ui::time::{Duration, Instant};

use crate::model::StreamSignal;

/// Marks an enrolled progress message as a live update.
pub const LIVE_PREFIX: &str = "tilo-live-v1:";
/// The wire version of [`AgentLiveUpdate`].
pub const LIVE_VERSION: u8 = 1;
/// The preview carries at most the last this many bytes of pending text.
pub const MAX_PREVIEW_BYTES: usize = 512;
/// The reply carries at most the last this many bytes of the reply so far.
pub const MAX_REPLY_BYTES: usize = 4 * 1024;
/// Label and tool name caps.
pub const MAX_LABEL_BYTES: usize = 120;
pub const MAX_TOOL_BYTES: usize = 64;
/// Target app and record caps.
pub const MAX_TARGET_BYTES: usize = 256;
/// The encoded message bound, below the enrolled carrier's 8000 characters
/// and 32000 bytes.
pub const MAX_MESSAGE_CHARS: usize = 7_900;
pub const MAX_MESSAGE_BYTES: usize = 31_000;
/// At most this many updates per second while the state is unchanged.
pub const MAX_UPDATES_PER_SECOND: u32 = 8;

/// What the agent is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveState {
    Thinking,
    Tool,
    Writing,
    Replying,
    Applying,
    Done,
    Failed,
}

impl LiveState {
    /// The wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Thinking => "thinking",
            Self::Tool => "tool",
            Self::Writing => "writing",
            Self::Replying => "replying",
            Self::Applying => "applying",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        [
            Self::Thinking,
            Self::Tool,
            Self::Writing,
            Self::Replying,
            Self::Applying,
            Self::Done,
            Self::Failed,
        ]
        .into_iter()
        .find(|state| state.as_str() == name)
    }
}

/// The chat reply so far.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveReply {
    pub text: String,
    pub done: bool,
}

/// Text written for a target that is not committed yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LivePreview {
    pub app: String,
    pub record: String,
    pub text: String,
}

/// One live update, as it travels after [`LIVE_PREFIX`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentLiveUpdate {
    pub v: u8,
    pub seq: u64,
    pub state: LiveState,
    pub label: String,
    pub tool: Option<String>,
    pub reply: Option<LiveReply>,
    pub preview: Option<LivePreview>,
}

/// A JSON string literal.
fn quote(text: &str) -> String {
    serde_json::Value::String(text.to_owned()).to_string()
}

/// The last at most `max` bytes of `text`, starting on a char boundary.
pub fn tail(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text.get(start..).unwrap_or_default()
}

/// The first at most `max` bytes of `text`, ending on a char boundary.
pub fn head(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.get(..end).unwrap_or_default()
}

impl AgentLiveUpdate {
    /// The progress message: [`LIVE_PREFIX`] + compact JSON, with every cap
    /// applied and within the enrolled progress bound.
    pub fn encode(&self) -> String {
        let mut update = self.clone();
        update.v = LIVE_VERSION;
        update.label = head(&update.label, MAX_LABEL_BYTES).to_owned();
        if let Some(tool) = &mut update.tool {
            *tool = head(tool, MAX_TOOL_BYTES).to_owned();
        }
        let mut reply_cap = MAX_REPLY_BYTES;
        let mut preview_cap = MAX_PREVIEW_BYTES;
        loop {
            if let Some(reply) = &mut update.reply {
                reply.text = tail(&reply.text, reply_cap).to_owned();
            }
            if let Some(preview) = &mut update.preview {
                preview.app = head(&preview.app, MAX_TARGET_BYTES).to_owned();
                preview.record = head(&preview.record, MAX_TARGET_BYTES).to_owned();
                preview.text = tail(&preview.text, preview_cap).to_owned();
            }
            let message = format!("{LIVE_PREFIX}{}", update.json());
            if message.chars().count() <= MAX_MESSAGE_CHARS && message.len() <= MAX_MESSAGE_BYTES {
                return message;
            }
            // JSON escaping (control characters become six bytes) can grow
            // the text past the bound: shorten the reply, then the preview.
            if reply_cap > 0 && update.reply.is_some() {
                reply_cap /= 2;
            } else if preview_cap > 0 && update.preview.is_some() {
                preview_cap /= 2;
            } else {
                update.reply = None;
                update.preview = None;
            }
        }
    }

    /// Compact JSON with the fields in wire order; absent options omitted.
    fn json(&self) -> String {
        let mut json = format!(
            "{{\"v\":{},\"seq\":{},\"state\":\"{}\",\"label\":{}",
            self.v,
            self.seq,
            self.state.as_str(),
            quote(&self.label)
        );
        if let Some(tool) = &self.tool {
            let _ = write!(json, ",\"tool\":{}", quote(tool));
        }
        if let Some(reply) = &self.reply {
            let _ = write!(
                json,
                ",\"reply\":{{\"text\":{},\"done\":{}}}",
                quote(&reply.text),
                reply.done
            );
        }
        if let Some(preview) = &self.preview {
            let _ = write!(
                json,
                ",\"preview\":{{\"app\":{},\"record\":{},\"text\":{}}}",
                quote(&preview.app),
                quote(&preview.record),
                quote(&preview.text)
            );
        }
        json.push('}');
        json
    }

    /// Parse a progress message written by [`Self::encode`]; `None` for any
    /// other message.
    pub fn decode(message: &str) -> Option<Self> {
        use serde_json::Value;
        if message.len() > MAX_MESSAGE_BYTES {
            return None;
        }
        let value: Value = serde_json::from_str(message.strip_prefix(LIVE_PREFIX)?).ok()?;
        let text = |value: &Value, key: &str| value.get(key)?.as_str().map(str::to_owned);
        let reply = match value.get("reply") {
            None => None,
            Some(reply) => Some(LiveReply {
                text: text(reply, "text")?,
                done: reply.get("done")?.as_bool()?,
            }),
        };
        let preview = match value.get("preview") {
            None => None,
            Some(preview) => Some(LivePreview {
                app: text(preview, "app")?,
                record: text(preview, "record")?,
                text: text(preview, "text")?,
            }),
        };
        Some(Self {
            v: u8::try_from(value.get("v")?.as_u64()?).ok()?,
            seq: value.get("seq")?.as_u64()?,
            state: LiveState::parse(value.get("state")?.as_str()?)?,
            label: text(&value, "label")?,
            tool: match value.get("tool") {
                None => None,
                Some(tool) => Some(tool.as_str()?.to_owned()),
            },
            reply,
            preview,
        })
    }
}

/// The label shown while a discovery tool call runs; `None` for `plan` and
/// `invoke`, whose activity is labelled from the operation's own descriptor.
pub fn tool_label(name: &str) -> Option<&'static str> {
    match name {
        crate::tools::PLAN | crate::tools::INVOKE => None,
        crate::tools::LIST_APPLICATIONS => Some("Looking up applications"),
        crate::tools::LIST_OPERATIONS => Some("Looking up operations"),
        crate::tools::DESCRIBE_OPERATION => Some("Reading an operation"),
        _ => Some("Using a tool"),
    }
}

/// The label for an operation being written or applied, from its own
/// descriptor: its action label and the label of the resource it acts on.
pub fn action_label(action_label: &str, resource_label: &str) -> String {
    let label = if resource_label.is_empty() {
        action_label.to_owned()
    } else {
        format!("{action_label}: {resource_label}")
    };
    head(&label, MAX_LABEL_BYTES).to_owned()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct View {
    state: Option<LiveState>,
    label: String,
    tool: Option<String>,
    reply: Option<LiveReply>,
    preview: Option<LivePreview>,
}

/// Coalesces live updates to at most [`MAX_UPDATES_PER_SECOND`] (the latest
/// value wins) while always sending a state change at once. It does not own
/// the transport: `tick`/`flush` take the send closure, which returns whether
/// the update was delivered. A failed send turns the publisher off for the
/// rest of the task: live updates are best effort and never stop the work.
#[derive(Debug, Default)]
pub struct LivePublisher {
    active: bool,
    view: View,
    seq: u64,
    last_sent: Option<(Instant, View)>,
    force: bool,
}

impl LivePublisher {
    /// Start publishing for one task.
    pub fn start(&mut self) {
        *self = Self {
            active: true,
            ..Self::default()
        };
    }

    /// Stop publishing (the task ended).
    pub fn stop(&mut self) {
        self.active = false;
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    /// The smallest gap between two updates in one state.
    pub fn interval() -> Duration {
        Duration::from_secs(1) / MAX_UPDATES_PER_SECOND
    }

    /// Enter `state`. A change of state is sent at the next tick regardless
    /// of the interval; leaving writing/applying drops the preview.
    pub fn set_state(&mut self, state: LiveState, label: &str, tool: Option<&str>) {
        if self.view.state != Some(state) {
            self.force = true;
            if !matches!(state, LiveState::Writing | LiveState::Applying) {
                self.view.preview = None;
            }
        }
        self.view.state = Some(state);
        head(label, MAX_LABEL_BYTES).clone_into(&mut self.view.label);
        self.view.tool = tool.map(|tool| head(tool, MAX_TOOL_BYTES).to_owned());
    }

    /// The reply so far (kept on every later update).
    pub fn set_reply(&mut self, text: &str, done: bool) {
        self.view.reply = Some(LiveReply {
            text: tail(text, MAX_REPLY_BYTES).to_owned(),
            done,
        });
    }

    pub fn set_preview(&mut self, preview: Option<LivePreview>) {
        self.view.preview = preview.map(|mut preview| {
            preview.text = tail(&preview.text, MAX_PREVIEW_BYTES).to_owned();
            preview
        });
    }

    fn changed(&self) -> bool {
        self.view.state.is_some()
            && self
                .last_sent
                .as_ref()
                .is_none_or(|(_, sent)| *sent != self.view)
    }

    /// Send the current view if it changed and either the state changed or
    /// the interval since the last update has passed.
    pub fn tick(&mut self, now: Instant, send: &mut dyn FnMut(String) -> bool) {
        let due = self.force
            || self
                .last_sent
                .as_ref()
                .is_none_or(|(at, _)| now.saturating_duration_since(*at) >= Self::interval());
        if due {
            self.flush(now, send);
        }
    }

    /// Send the current view now if it changed.
    pub fn flush(&mut self, now: Instant, send: &mut dyn FnMut(String) -> bool) {
        if !self.active || !self.changed() {
            return;
        }
        let Some(state) = self.view.state else {
            return;
        };
        self.seq = self.seq.saturating_add(1);
        let update = AgentLiveUpdate {
            v: LIVE_VERSION,
            seq: self.seq,
            state,
            label: self.view.label.clone(),
            tool: self.view.tool.clone(),
            reply: self.view.reply.clone(),
            preview: self.view.preview.clone(),
        };
        self.force = false;
        self.last_sent = Some((now, self.view.clone()));
        if !send(update.encode()) {
            self.active = false;
        }
    }
}

/// Where a planned operation writes and how it is shown, from its descriptor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlanTarget {
    pub app: String,
    pub record: String,
    /// [`action_label`] of the operation.
    pub label: String,
    /// The names of its `Text`-typed parameters: the arguments previewed.
    pub text_params: Vec<String>,
}

/// Maps the model's stream onto live states while a plan is being chosen.
/// `targets` resolves every operation id the plan may name to its
/// [`PlanTarget`].
#[derive(Debug, Default)]
pub struct PlanStream {
    targets: BTreeMap<String, PlanTarget>,
    steps: BTreeMap<usize, PlanTarget>,
    /// The `invoke` calls of the current turn, by call index.
    calls: BTreeMap<usize, PlanTarget>,
}

impl PlanStream {
    pub fn new(targets: BTreeMap<String, PlanTarget>) -> Self {
        Self {
            targets,
            steps: BTreeMap::new(),
            calls: BTreeMap::new(),
        }
    }

    /// Replace the operations a call may name (the catalogue moved) and
    /// forget the previous turn's calls.
    pub fn retarget(&mut self, targets: BTreeMap<String, PlanTarget>) {
        self.targets = targets;
        self.steps.clear();
        self.calls.clear();
    }

    /// The operation target the `invoke` call at `call` named, once known.
    pub fn call_target(&self, call: usize) -> Option<&PlanTarget> {
        self.calls.get(&call)
    }

    /// Fold one signal into `live`.
    pub fn apply(&mut self, live: &mut LivePublisher, signal: StreamSignal<'_>) {
        match signal {
            StreamSignal::Turn(_) => live.set_state(LiveState::Thinking, "Thinking", None),
            StreamSignal::ToolCall(name) => {
                if let Some(label) = tool_label(name) {
                    live.set_state(LiveState::Tool, label, Some(name));
                }
            }
            StreamSignal::Reply { text, done } => {
                live.set_state(LiveState::Replying, "Replying", None);
                live.set_reply(text, done);
            }
            StreamSignal::StepOperation { step, operation } => {
                if let Some(target) = self.targets.get(operation) {
                    self.steps.insert(step, target.clone());
                }
            }
            StreamSignal::StepArgument {
                step, name, text, ..
            } => {
                // Only a Text parameter of a known operation is previewed.
                let Some(target) = self
                    .steps
                    .get(&step)
                    .filter(|target| target.text_params.iter().any(|param| param == name))
                else {
                    return;
                };
                live.set_state(LiveState::Writing, &target.label, None);
                live.set_preview(Some(LivePreview {
                    app: target.app.clone(),
                    record: target.record.clone(),
                    text: text.to_owned(),
                }));
            }
            StreamSignal::CallOperation {
                call, operation, ..
            } => {
                if let Some(target) = self.targets.get(operation) {
                    self.calls.insert(call, target.clone());
                }
            }
            StreamSignal::CallArgument {
                call, name, text, ..
            } => {
                let Some(target) = self
                    .calls
                    .get(&call)
                    .filter(|target| target.text_params.iter().any(|param| param == name))
                else {
                    return;
                };
                live.set_state(LiveState::Writing, &target.label, None);
                live.set_preview(Some(LivePreview {
                    app: target.app.clone(),
                    record: target.record.clone(),
                    text: text.to_owned(),
                }));
            }
        }
    }
}

/// Contract tag prefix by which an app declares that a Text parameter may be
/// delivered in parts: `tilo.incremental-text:<param>:<next>` means the first
/// part goes through the declaring action and each later part through action
/// `<next>` on the same resource with `<param>` set to that part (Writer's
/// `INCREMENTAL_TEXT_TAG`). The agent keys chunking on this and nothing else.
pub const INCREMENTAL_TEXT_TAG: &str = "tilo.incremental-text:";

/// The `(param, next action)` a contract's tags declare, if any.
pub fn incremental_text(tags: &[String]) -> Option<(&str, &str)> {
    tags.iter().find_map(|tag| {
        let (param, next) = tag.strip_prefix(INCREMENTAL_TEXT_TAG)?.split_once(':')?;
        (!param.is_empty() && !next.is_empty() && !next.contains(':')).then_some((param, next))
    })
}

/// A declared incremental text longer than this is written with a live
/// preview of the text still to land.
pub const CHUNK_MIN_TEXT: usize = 60;
/// The largest chunk target the adaptive doubling of streamed chunks reaches.
pub const MAX_CHUNK_TARGET: usize = 4096;
/// At most this many chunks per paragraph.
pub const MAX_CHUNKS: usize = 12;
/// A streamed chunk step slower than this doubles the next chunk's target.
pub const SLOW_CHUNK_STEP: Duration = Duration::from_millis(600);
/// The most text one step carries, measured as its JSON string encoding.
/// A step's arguments cross the broker as one JSON object of at most 32 KiB
/// (`tilo-action-broker` `DIRECTORY_MAX_PARAMS_BYTES`); this leaves room for
/// the other arguments around the text.
pub const MAX_STEP_TEXT_JSON: usize = 24 * 1024;

/// The bytes `c` takes inside a JSON string: quote, backslash and the named
/// control escapes take two, other control characters six (`\u00XX`), and
/// everything else its UTF-8 length.
fn json_string_len(c: char) -> usize {
    match c {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{08}' | '\u{0c}' => 2,
        c if u32::from(c) < 0x20 => 6,
        c => c.len_utf8(),
    }
}

/// The byte length of the next step's part of `rest`, a text that is
/// already complete: all of it in one step, unless its JSON encoding would
/// exceed [`MAX_STEP_TEXT_JSON`] and more than one step is left — then the
/// longest prefix within that bound, ending just after whitespace when there
/// is any. The parts concatenate back to exactly the input.
///
/// Complete text is never split to look like typing: each step is a
/// durable round trip, so a paragraph in parts only lands later.
pub fn step_text_len(rest: &str, steps_left: usize) -> usize {
    if steps_left <= 1 {
        return rest.len();
    }
    let mut encoded = 0_usize;
    let mut boundary = None;
    for (at, c) in rest.char_indices() {
        encoded += json_string_len(c);
        if encoded > MAX_STEP_TEXT_JSON {
            return boundary.unwrap_or(at);
        }
        if c.is_whitespace() {
            boundary = Some(at + c.len_utf8());
        }
    }
    rest.len()
}

/// The byte length of the next chunk of `rest`: the whole rest when it is
/// short or `chunks_left` is 1; otherwise the last word boundary (just after
/// whitespace) between 2/3 and 4/3 of `target`, else the first boundary after
/// that window, else everything. A tail shorter than a third of `target` is
/// folded into this chunk. Chunks concatenate back to exactly the input.
pub fn next_chunk_len(rest: &str, target: usize, chunks_left: usize) -> usize {
    let target = target.max(3);
    let (min, max) = (target - target / 3, target + target / 3);
    if chunks_left <= 1 || rest.len() <= max {
        return rest.len();
    }
    let mut inside = None;
    let mut after = None;
    for (at, c) in rest.char_indices() {
        if !c.is_whitespace() {
            continue;
        }
        let end = at + c.len_utf8();
        if end > max {
            after = Some(end);
            break;
        }
        if end >= min {
            inside = Some(end);
        }
    }
    let cut = inside.or(after).unwrap_or(rest.len());
    if rest.len() - cut < target / 3 {
        rest.len()
    } else {
        cut
    }
}

/// `text` split with a fixed target (no adaptation), as [`next_chunk_len`]
/// would split it; at most `most` chunks.
pub fn split_chunks(text: &str, target: usize, most: usize) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let len = next_chunk_len(rest, target, most.saturating_sub(chunks.len()));
        let (chunk, after) = rest.split_at(len);
        chunks.push(chunk);
        rest = after;
    }
    chunks
}

/// The next chunk target after a step that took `elapsed`.
pub fn adapt_chunk_target(target: usize, elapsed: Duration) -> usize {
    if elapsed > SLOW_CHUNK_STEP {
        target.saturating_mul(2).min(MAX_CHUNK_TARGET)
    } else {
        target
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OCEAN: &str = "The ocean covers more than seventy percent of our planet, holding \
        ninety-seven percent of its water and most of its life, from sunlit reefs \
        to the dark trenches where strange creatures glow in the deep.";
    /// A streamed chunk target: chunks land between 2/3 and 4/3 of it.
    const CHUNK_TARGET: usize = 60;

    /// Complete text is one step, however long, until one step's JSON bound;
    /// past it, parts end at a word boundary within the bound and rejoin
    /// exactly.
    #[test]
    fn complete_text_is_one_step_up_to_the_step_bound() {
        let paragraph = "word ".repeat(140);
        assert_eq!(paragraph.len(), 700);
        assert_eq!(step_text_len(&paragraph, MAX_CHUNKS), paragraph.len());
        assert_eq!(step_text_len("", MAX_CHUNKS), 0);

        let long = "word ".repeat(6000);
        let first = step_text_len(&long, MAX_CHUNKS);
        assert!(first <= MAX_STEP_TEXT_JSON && first > MAX_STEP_TEXT_JSON - 5);
        assert!(long[..first].ends_with(' '));
        assert_eq!(
            step_text_len(&long[first..], MAX_CHUNKS),
            long.len() - first,
            "the rest fits one step"
        );
        assert_eq!(
            step_text_len(&long, 1),
            long.len(),
            "the last step takes all"
        );

        // The bound is on the encoding: escapes count, multibyte characters
        // are never cut.
        let quoted = "\"".repeat(MAX_STEP_TEXT_JSON);
        let cut = step_text_len(&quoted, MAX_CHUNKS);
        assert_eq!(cut, MAX_STEP_TEXT_JSON / 2);
        assert!(serde_json::to_string(&quoted[..cut]).unwrap().len() <= MAX_STEP_TEXT_JSON + 2);
        let control = "\u{1}".repeat(MAX_STEP_TEXT_JSON);
        assert_eq!(step_text_len(&control, MAX_CHUNKS), MAX_STEP_TEXT_JSON / 6);
        let accents = "é".repeat(MAX_STEP_TEXT_JSON);
        let cut = step_text_len(&accents, MAX_CHUNKS);
        assert!(accents.is_char_boundary(cut) && cut <= MAX_STEP_TEXT_JSON);
    }

    #[test]
    fn chunks_split_at_word_boundaries_within_the_window_and_rejoin_exactly() {
        let chunks = split_chunks(OCEAN, CHUNK_TARGET, MAX_CHUNKS);
        assert_eq!(chunks.concat(), OCEAN);
        assert!(chunks.len() > 1 && chunks.len() <= MAX_CHUNKS);
        for chunk in &chunks[..chunks.len() - 1] {
            assert!(
                (40..=80).contains(&chunk.len()),
                "{chunk:?} {}",
                chunk.len()
            );
            assert!(chunk.ends_with(' '), "{chunk:?} ends at a word boundary");
        }
        // Short texts and the last allowed chunk are taken whole.
        assert_eq!(
            split_chunks("short text", CHUNK_TARGET, MAX_CHUNKS),
            ["short text"]
        );
        assert_eq!(split_chunks(OCEAN, CHUNK_TARGET, 1), [OCEAN]);
    }

    #[test]
    fn chunks_are_capped_and_handle_unbroken_and_multibyte_text() {
        let long = "word ".repeat(400);
        let chunks = split_chunks(&long, CHUNK_TARGET, MAX_CHUNKS);
        assert_eq!(chunks.len(), MAX_CHUNKS);
        assert_eq!(chunks.concat(), long);
        let unbroken = "x".repeat(300);
        assert_eq!(
            split_chunks(&unbroken, CHUNK_TARGET, MAX_CHUNKS),
            [unbroken.as_str()]
        );
        let accents = "été ".repeat(40);
        let chunks = split_chunks(&accents, CHUNK_TARGET, MAX_CHUNKS);
        assert_eq!(chunks.concat(), accents);
        assert!(chunks.iter().all(|chunk| !chunk.is_empty()));
        // A tiny tail joins the chunk before it.
        let text = format!("{} end", "abcd ".repeat(15));
        let chunks = split_chunks(&text, CHUNK_TARGET, MAX_CHUNKS);
        assert!(
            chunks.last().unwrap().len() >= CHUNK_TARGET / 3,
            "{chunks:?}"
        );
    }

    #[test]
    fn incremental_text_is_read_only_from_the_declared_tag() {
        let tags = |list: &[&str]| list.iter().map(|tag| (*tag).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            incremental_text(&tags(&[
                "other",
                "tilo.incremental-text:text:continue-paragraph"
            ])),
            Some(("text", "continue-paragraph"))
        );
        for bad in [
            "tilo.incremental-text:",
            "tilo.incremental-text:text",
            "tilo.incremental-text::next",
            "tilo.incremental-text:text:",
            "tilo.incremental-text:a:b:c",
            "incremental-text:text:next",
        ] {
            assert_eq!(incremental_text(&tags(&[bad])), None, "{bad}");
        }
        assert_eq!(
            INCREMENTAL_TEXT_TAG,
            word_processor::actions::INCREMENTAL_TEXT_TAG,
            "the agent reads the tag Writer declares"
        );
    }

    #[test]
    fn a_slow_step_doubles_the_next_chunk_target() {
        assert_eq!(adapt_chunk_target(60, Duration::from_millis(599)), 60);
        assert_eq!(adapt_chunk_target(60, Duration::from_millis(601)), 120);
        assert_eq!(
            adapt_chunk_target(4000, Duration::from_secs(2)),
            MAX_CHUNK_TARGET
        );
        let rest = OCEAN;
        let small = next_chunk_len(rest, 60, MAX_CHUNKS);
        let large = next_chunk_len(rest, 120, MAX_CHUNKS);
        assert!(
            large > small && (80..=160).contains(&large),
            "{small} {large}"
        );
    }

    fn collect(live: &mut LivePublisher, now: Instant, sent: &mut Vec<AgentLiveUpdate>) {
        live.tick(now, &mut |message| {
            sent.push(AgentLiveUpdate::decode(&message).expect("a live message"));
            true
        });
    }

    #[test]
    fn updates_encode_compactly_with_the_prefix_and_round_trip() {
        let update = AgentLiveUpdate {
            v: 1,
            seq: 7,
            state: LiveState::Writing,
            label: "Writing in Writer".into(),
            tool: Some("list_operations".into()),
            reply: Some(LiveReply {
                text: "Sure — writing it now".into(),
                done: false,
            }),
            preview: Some(LivePreview {
                app: "so.tilo.word-processor".into(),
                record: String::new(),
                text: "The ocean covers".into(),
            }),
        };
        let message = update.encode();
        assert_eq!(
            message,
            "tilo-live-v1:{\"v\":1,\"seq\":7,\"state\":\"writing\",\"label\":\"Writing in Writer\",\
             \"tool\":\"list_operations\",\"reply\":{\"text\":\"Sure — writing it now\",\"done\":false},\
             \"preview\":{\"app\":\"so.tilo.word-processor\",\"record\":\"\",\"text\":\"The ocean covers\"}}"
        );
        assert_eq!(AgentLiveUpdate::decode(&message), Some(update));
        assert_eq!(AgentLiveUpdate::decode("Running 1 of 2: Append"), None);
        let bare = AgentLiveUpdate {
            v: 1,
            seq: 1,
            state: LiveState::Thinking,
            label: "Thinking".into(),
            tool: None,
            reply: None,
            preview: None,
        };
        assert_eq!(
            bare.encode(),
            r#"tilo-live-v1:{"v":1,"seq":1,"state":"thinking","label":"Thinking"}"#
        );
    }

    #[test]
    fn caps_keep_tails_on_char_boundaries_and_the_carrier_bound() {
        let long = "é".repeat(5000); // 10000 bytes
        let update = AgentLiveUpdate {
            v: 1,
            seq: 1,
            state: LiveState::Writing,
            label: "x".repeat(500),
            tool: None,
            reply: Some(LiveReply {
                text: format!("start{long}end"),
                done: false,
            }),
            preview: Some(LivePreview {
                app: "so.tilo.word-processor".into(),
                record: String::new(),
                text: format!("{long}🌊"),
            }),
        };
        let decoded = AgentLiveUpdate::decode(&update.encode()).unwrap();
        let reply = decoded.reply.unwrap().text;
        assert!(reply.len() <= MAX_REPLY_BYTES && reply.ends_with("end"));
        let preview = decoded.preview.unwrap().text;
        assert!(preview.len() <= MAX_PREVIEW_BYTES && preview.ends_with('🌊'));
        assert_eq!(decoded.label.len(), MAX_LABEL_BYTES);
        // Control characters escape to six bytes each: still within bound.
        let hostile = AgentLiveUpdate {
            reply: Some(LiveReply {
                text: "\u{1}".repeat(MAX_REPLY_BYTES),
                done: false,
            }),
            preview: Some(LivePreview {
                app: "so.tilo.word-processor".into(),
                record: "r".into(),
                text: "\u{2}".repeat(MAX_PREVIEW_BYTES),
            }),
            ..update
        };
        let message = hostile.encode();
        assert!(
            message.chars().count() <= MAX_MESSAGE_CHARS,
            "{}",
            message.len()
        );
        assert!(message.len() <= MAX_MESSAGE_BYTES);
        assert!(AgentLiveUpdate::decode(&message).is_some());
        assert_eq!(tail("aé", 1), "");
        assert_eq!(tail("aé", 2), "é");
        assert_eq!(head("éa", 1), "");
    }

    #[test]
    fn the_publisher_coalesces_to_the_latest_value_and_sends_state_changes_at_once() {
        let mut live = LivePublisher::default();
        live.start();
        let t0 = ui::time::now();
        let mut sent = Vec::new();
        live.set_state(LiveState::Replying, "Replying", None);
        for (ms, text) in [(0, "S"), (20, "Su"), (40, "Sur"), (60, "Sure")] {
            live.set_reply(text, false);
            collect(&mut live, t0 + Duration::from_millis(ms), &mut sent);
        }
        // The first went at once; the rest waited for the 125 ms window.
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].reply.as_ref().unwrap().text, "S");
        collect(&mut live, t0 + Duration::from_millis(130), &mut sent);
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1].reply.as_ref().unwrap().text, "Sure", "latest wins");
        // A state change goes out immediately, inside the window, and keeps
        // the reply; nothing unchanged is sent twice.
        live.set_state(
            LiveState::Tool,
            "Looking up operations",
            Some("list_operations"),
        );
        collect(&mut live, t0 + Duration::from_millis(131), &mut sent);
        collect(&mut live, t0 + Duration::from_millis(400), &mut sent);
        assert_eq!(sent.len(), 3);
        assert_eq!(sent[2].state, LiveState::Tool);
        assert_eq!(sent[2].tool.as_deref(), Some("list_operations"));
        assert_eq!(sent[2].reply.as_ref().unwrap().text, "Sure");
        assert_eq!(
            sent.iter().map(|update| update.seq).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        // Over one second in one state, at most eight updates go out.
        let mut sent = Vec::new();
        live.set_state(LiveState::Writing, "Writing", None);
        for ms in 0..1000_u64 {
            live.set_preview(Some(LivePreview {
                app: "so.tilo.word-processor".into(),
                record: String::new(),
                text: format!("{ms}"),
            }));
            collect(&mut live, t0 + Duration::from_millis(1000 + ms), &mut sent);
        }
        assert!(sent.len() <= 9, "{}", sent.len());
    }

    #[test]
    fn a_failed_send_turns_the_publisher_off_and_an_inactive_one_sends_nothing() {
        let mut live = LivePublisher::default();
        live.set_state(LiveState::Thinking, "Thinking", None);
        let mut calls = 0;
        live.flush(ui::time::now(), &mut |_| {
            calls += 1;
            true
        });
        assert_eq!(calls, 0, "never started");
        live.start();
        live.set_state(LiveState::Thinking, "Thinking", None);
        live.flush(ui::time::now(), &mut |_| {
            calls += 1;
            false
        });
        live.set_state(LiveState::Done, "Done", None);
        live.flush(ui::time::now(), &mut |_| {
            calls += 1;
            true
        });
        assert_eq!(calls, 1);
        assert!(!live.is_active());
    }

    #[test]
    fn the_plan_stream_maps_signals_to_states_and_previews() {
        let mut live = LivePublisher::default();
        live.start();
        let mut stream = PlanStream::new(BTreeMap::from([(
            "broker:3:word-processor.document#append-paragraph".to_owned(),
            PlanTarget {
                app: "so.tilo.word-processor".to_owned(),
                record: String::new(),
                label: action_label("Append paragraph", "Document"),
                text_params: vec!["text".to_owned()],
            },
        )]));
        let mut sent = Vec::new();
        let mut at = ui::time::now();
        let mut feed = |live: &mut LivePublisher, stream: &mut PlanStream, signal| {
            stream.apply(live, signal);
            at += Duration::from_millis(200);
            collect(live, at, &mut sent);
        };
        feed(&mut live, &mut stream, StreamSignal::Turn(0));
        feed(
            &mut live,
            &mut stream,
            StreamSignal::ToolCall("list_operations"),
        );
        feed(&mut live, &mut stream, StreamSignal::ToolCall("plan"));
        feed(
            &mut live,
            &mut stream,
            StreamSignal::Reply {
                text: "On it",
                done: true,
            },
        );
        feed(
            &mut live,
            &mut stream,
            StreamSignal::StepOperation {
                step: 0,
                operation: "broker:3:word-processor.document#append-paragraph",
            },
        );
        // A non-Text argument is never previewed.
        feed(
            &mut live,
            &mut stream,
            StreamSignal::StepArgument {
                step: 0,
                name: "style",
                text: "plain",
                done: true,
            },
        );
        feed(
            &mut live,
            &mut stream,
            StreamSignal::StepArgument {
                step: 0,
                name: "text",
                text: "The ocean",
                done: false,
            },
        );
        let states: Vec<_> = sent.iter().map(|update| update.state).collect();
        assert_eq!(
            states,
            [
                LiveState::Thinking,
                LiveState::Tool,
                LiveState::Replying,
                LiveState::Writing
            ]
        );
        assert_eq!(sent[1].label, "Looking up operations");
        assert_eq!(sent[3].label, "Append paragraph: Document");
        let preview = sent[3].preview.as_ref().unwrap();
        assert_eq!(
            (
                preview.app.as_str(),
                preview.record.as_str(),
                preview.text.as_str()
            ),
            ("so.tilo.word-processor", "", "The ocean")
        );
        assert_eq!(sent[3].reply.as_ref().unwrap().text, "On it");
    }

    /// An `invoke` call's Text argument previews at the operation's own
    /// target while it streams; `invoke` itself shows no tool label.
    #[test]
    fn an_invoke_call_previews_its_text_argument() {
        let mut live = LivePublisher::default();
        live.start();
        let id = "broker:3:word-processor.document#type-text";
        let mut stream = PlanStream::new(BTreeMap::from([(
            id.to_owned(),
            PlanTarget {
                app: "so.tilo.word-processor".to_owned(),
                record: "doc-1".to_owned(),
                label: action_label("Type text", "Brief"),
                text_params: vec!["text".to_owned()],
            },
        )]));
        stream.apply(&mut live, StreamSignal::ToolCall(crate::tools::INVOKE));
        stream.apply(
            &mut live,
            StreamSignal::CallArgument {
                call: 1,
                name: "text",
                text: "too early",
                done: false,
            },
        );
        let mut sent = Vec::new();
        collect(&mut live, ui::time::now(), &mut sent);
        assert!(sent.is_empty(), "nothing known yet: {sent:?}");
        stream.apply(
            &mut live,
            StreamSignal::CallOperation {
                call: 1,
                operation: id,
                done: true,
            },
        );
        stream.apply(
            &mut live,
            StreamSignal::CallArgument {
                call: 1,
                name: "text",
                text: "The ocean",
                done: false,
            },
        );
        collect(&mut live, ui::time::now(), &mut sent);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].state, LiveState::Writing);
        assert_eq!(sent[0].label, "Type text: Brief");
        let preview = sent[0].preview.as_ref().unwrap();
        assert_eq!(
            (
                preview.app.as_str(),
                preview.record.as_str(),
                preview.text.as_str()
            ),
            ("so.tilo.word-processor", "doc-1", "The ocean")
        );
        assert_eq!(stream.call_target(1).unwrap().label, "Type text: Brief");
    }
}
