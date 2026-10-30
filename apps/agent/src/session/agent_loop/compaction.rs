//! A long run keeps within its model's context.
//!
//! A run of many items reads far more than any model holds at once: eight
//! mails, their attachments and the pages behind their links. Nothing the
//! run read is dropped, though. Before each turn the conversation is
//! measured against the model's context, and while it does not fit, the
//! oldest tool results the model has already acted on are put aside: each is
//! kept whole in the run's pages and the model is sent its handle and a short
//! summary instead, which `read_result` reads again from the start. The most
//! recent results always stay whole, since the next step is about them, and
//! pictures already looked at give way to a note. When even that is too
//! much (a run of hundreds of turns), the oldest turns fold into one note
//! closing the request: a line per step with its result's handle, the
//! newest lines kept, so what the model is sent stays bounded however long
//! the run.
//!
//! The history the conversation validates keeps every result as it was; a
//! compaction changes only what the model is sent.

use std::fmt::Write as _;

use serde_json::{Value, json};

use crate::model::Conversation;
use crate::pages::{Pages, READ_RESULT};

/// The context assumed for a model whose descriptor does not say: a small
/// one, so a run on an unknown model compacts early rather than overflows.
pub(crate) const UNKNOWN_CONTEXT_TOKENS: u32 = 32_000;
/// Characters a token stands for at least, in the JSON the model is sent.
/// Text in English is about four; JSON punctuation and identifiers are
/// fewer, so three keeps the estimate above the provider's count.
const CHARS_PER_TOKEN: usize = 3;
/// Tokens of the context left for the model's answer and for the results
/// of the turn about to run.
const ANSWER_ROOM_TOKENS: usize = 4_096 + 4_096;
/// The share of the context the conversation may fill before older results
/// are put aside, in percent.
const FILL_PERCENT: usize = 60;
/// The most recent tool results that always stay whole.
const KEEP_RECENT: usize = 6;
/// The characters of a result its summary keeps.
const SUMMARY_CHARS: usize = 280;
/// A result shorter than this is not worth a handle: its summary would be
/// as long.
const WORTH_PUTTING_ASIDE: usize = 600;
/// The most recent turns a fold never takes.
const KEEP_RECENT_TURNS: usize = 4;
/// The most steps a fold's note lists, the newest; older ones are counted.
const MOST_NOTED: usize = 120;
/// What a picture already looked at becomes.
const PICTURE_ASIDE: &str = "(A picture shown earlier in this task, put aside to keep the \
     conversation short. Invoke its operation again to look at it.)";

/// The model's context, as compaction measures it, and what the run put
/// aside so far.
#[derive(Debug)]
pub(crate) struct Context {
    /// The most tokens the conversation may take before older results are
    /// put aside.
    fill_tokens: usize,
    /// The handle each result put aside is kept under, by call id.
    handles: std::collections::HashMap<String, String>,
}

impl Context {
    /// The context of a model reading `tokens` at once, when it is known.
    pub(crate) fn of(tokens: Option<u32>) -> Self {
        let tokens =
            usize::try_from(tokens.unwrap_or(UNKNOWN_CONTEXT_TOKENS)).unwrap_or(usize::MAX);
        let usable = tokens.saturating_sub(ANSWER_ROOM_TOKENS);
        Self {
            fill_tokens: usable.saturating_mul(FILL_PERCENT) / 100,
            handles: std::collections::HashMap::new(),
        }
    }

    /// The tokens a request body takes, estimated from above.
    pub(crate) fn tokens_of(body: &Value) -> usize {
        body.to_string().len().div_ceil(CHARS_PER_TOKEN)
    }

    /// Put aside the oldest results until what the model is sent fits, the
    /// most recent [`KEEP_RECENT`] excepted. Returns how many were put aside.
    pub(crate) fn compact(&mut self, conversation: &mut Conversation, pages: &mut Pages) -> usize {
        let tools = crate::tools::typed_loop_definitions();
        let measure = |conversation: &Conversation| Self::tokens_of(&conversation.body(&tools));
        let mut tokens = measure(conversation);
        if tokens <= self.fill_tokens {
            return 0;
        }
        if conversation.put_pictures_aside(PICTURE_ASIDE) > 0 {
            tokens = measure(conversation);
            if tokens <= self.fill_tokens {
                return 0;
            }
        }
        // Each result put aside saves what it was less what replaces it,
        // counted without building the request again.
        let mut aside = 0;
        let answered = conversation.answered();
        let older = answered.len().saturating_sub(KEEP_RECENT);
        for call in answered.iter().take(older) {
            if tokens <= self.fill_tokens {
                break;
            }
            if conversation.is_aside(&call.call_id)
                || call.result.chars().count() < WORTH_PUTTING_ASIDE
            {
                continue;
            }
            let handle = self.handle(pages, call);
            let replacement = put_aside(call, &handle);
            tokens = tokens.saturating_sub(
                call.result
                    .len()
                    .saturating_sub(replacement.len())
                    .div_ceil(CHARS_PER_TOKEN),
            );
            conversation.put_aside(&call.call_id, replacement);
            aside += 1;
        }
        tokens = measure(conversation);
        // Still too much: fold the oldest turns, an eighth of what may be
        // folded at a time, measured after each.
        let starts: Vec<usize> = conversation
            .turn_starts()
            .into_iter()
            .filter(|through| *through > conversation.folded_through())
            .collect();
        let foldable = starts
            .len()
            .saturating_sub(KEEP_RECENT_TURNS.saturating_sub(1));
        let step = foldable.div_ceil(8).max(1);
        let mut next = step;
        while tokens > self.fill_tokens && next <= foldable && foldable > 0 {
            let Some(&through) = starts.get(next.min(foldable) - 1) else {
                break;
            };
            let note = self.note(pages, &answered, through);
            conversation.fold(through, note);
            tokens = measure(conversation);
            next += step;
        }
        tracing::info!(
            aside,
            tokens,
            folded_through = conversation.folded_through(),
            "compacted the run's conversation"
        );
        aside
    }

    /// The handle `call`'s result is kept under, keeping it now if it is
    /// not yet.
    fn handle(&mut self, pages: &mut Pages, call: &crate::model::Answered) -> String {
        self.handles
            .entry(call.call_id.clone())
            .or_insert_with(|| pages.keep_aside(&call.result))
            .clone()
    }

    /// The note the turns before `through` fold into: a line per step with
    /// its result's handle, the newest [`MOST_NOTED`].
    fn note(
        &mut self,
        pages: &mut Pages,
        answered: &[crate::model::Answered],
        through: usize,
    ) -> String {
        let folded: Vec<&crate::model::Answered> =
            answered.iter().filter(|call| call.at < through).collect();
        let earlier = folded.len().saturating_sub(MOST_NOTED);
        let mut note = format!(
            "Earlier steps of this task, put aside to keep the conversation short. Each \
             result is kept: call `{READ_RESULT}` with its `result` and `from` 0 to read it \
             again."
        );
        if earlier > 0 {
            let _ = write!(note, "\n- {earlier} earlier steps");
        }
        for call in folded.into_iter().skip(earlier) {
            let handle = self.handle(pages, call);
            let what = call
                .input
                .get("operation")
                .and_then(Value::as_str)
                .map_or_else(
                    || call.tool.clone(),
                    |operation| format!("{} {operation}", call.tool),
                );
            let _ = write!(note, "\n- {what}: result {handle}");
        }
        note
    }
}

/// What a result put aside is sent as: its handle, what it answered and the
/// start of what it said.
fn put_aside(call: &crate::model::Answered, handle: &str) -> String {
    let mut summary: String = call.result.chars().take(SUMMARY_CHARS).collect();
    summary.push('…');
    let mut aside = json!({
        "put_aside": true,
        "result": handle,
        "from": 0,
        "total": call.result.chars().count(),
        "tool": call.tool,
        "summary": summary,
        "how": format!(
            "Read earlier in this task and put aside to keep the conversation short. Call \
             `{READ_RESULT}` with this `result` and `from` 0 to read it again."
        ),
    });
    if let Some(operation) = call.input.get("operation").and_then(Value::as_str)
        && let Some(object) = aside.as_object_mut()
    {
        object.insert("operation".into(), Value::String(operation.to_owned()));
    }
    aside.to_string()
}
