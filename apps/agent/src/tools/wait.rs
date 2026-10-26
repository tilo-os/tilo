//! A plan that waits for what someone else will send: the `plan`
//! property a waiting planner offers ([`typed_waiting_definitions`]) and
//! how it is read ([`Wait`]).

use agent_provider_config::protocol::ToolDefinition;
use serde_json::{Value, json};

use super::{PLAN, typed_definitions};

/// The optional `plan` property a planner that can wait offers
/// ([`typed_waiting_definitions`]): a model that needs what someone else
/// will send (a login code, a reply) names it here instead of guessing or
/// giving up.
pub const WAIT_FOR: &str = "wait_for";

/// The longest one [`WAIT_FOR`] waits, in seconds: a wait holds the task
/// open, so it stays short and the model decides again after it.
pub const MOST_WAIT_SECONDS: u64 = 120;

/// The most characters of [`Wait::app`] and [`Wait::contains`] kept: a
/// place or a phrase, never a paragraph.
const MOST_WAIT_TEXT_CHARS: usize = 200;

/// A plan's [`WAIT_FOR`] as the model wrote it, before the planner resolves
/// its place to one of the apps messages arrive in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wait {
    /// Where the message arrives, as the model named it; `None` for any of
    /// the places messages arrive.
    pub app: Option<String>,
    /// Text the message holds, matched without regard to case; `None` for
    /// any message.
    pub contains: Option<String>,
    /// The longest it waits, within `1..=`[`MOST_WAIT_SECONDS`].
    pub seconds: u64,
}

/// The [`WAIT_FOR`] of `input` (a plan, or one item of its checklist), if
/// it names one: absent or null is none. Seconds out of range are brought
/// within it rather than refused, since the bound is the planner's to keep,
/// not the model's to know exactly.
///
/// # Errors
///
/// A `wait_for` that is not an object, lacks whole `seconds` or holds
/// something other than text where text goes.
pub fn read_wait(input: &Value) -> Result<Option<Wait>, String> {
    let wait = match input.get(WAIT_FOR) {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Object(wait)) => wait,
        Some(_) => return Err(format!("`{WAIT_FOR}` is an object, or left out")),
    };
    let seconds = wait
        .get("seconds")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("`{WAIT_FOR}` needs `seconds`, a whole number"))?
        .clamp(1, MOST_WAIT_SECONDS);
    let text = |key: &str| -> Result<Option<String>, String> {
        match wait.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => Ok(Some(text.trim())
                .filter(|text| !text.is_empty())
                .map(|text| text.chars().take(MOST_WAIT_TEXT_CHARS).collect())),
            Some(_) => Err(format!("`{WAIT_FOR}.{key}` is text")),
        }
    };
    Ok(Some(Wait {
        app: text("app")?,
        contains: text("contains")?,
        seconds,
    }))
}

/// The [`WAIT_FOR`] property of a waiting planner's `plan`: where the
/// message arrives (one of `places`), words it holds, and how long to wait.
/// The places are named in words, not as an `enum`, like every id in these
/// tools; the planner refuses one it does not know back to the model. Not
/// nullable, as no schema here is: left out is no wait.
fn wait_for_schema(places: &[&str], most_waits: u32) -> Value {
    json!({
        "type": "object",
        "description": format!(
            "Leave it out unless the request needs something another person or agent has \
             yet to send (a login code, a reply) that nothing shown holds. Then: wait for it \
             after the steps; you are asked again with what arrived, or told nothing did. One \
             wait lasts at most {MOST_WAIT_SECONDS} s, and a task may wait {most_waits} \
             times: if nothing arrived and it may still come, wait again."
        ),
        "properties": {
            "app": {
                "type": "string",
                "description": format!("Where it arrives: {}; omit for any.", places.join(", "))
            },
            "contains": {
                "type": "string",
                "description": "Words it will hold, e.g. \"code\" (any order and case; its \
                    sender's name counts); omit for any new message."
            },
            "seconds": {
                "type": "integer",
                "minimum": 1,
                "maximum": MOST_WAIT_SECONDS,
                "description": "The longest to wait."
            }
        },
        "required": ["seconds"],
        "additionalProperties": false
    })
}

/// [`typed_definitions`] for a planner that can hold a task until a message
/// arrives: its `plan` also takes [`WAIT_FOR`], naming one of `places`, and
/// says a task waits at most `most_waits` times. Only such a planner offers
/// it, since only it can keep the promise. A function of its arguments
/// alone, so byte-identical on every turn when they are constants.
pub fn typed_waiting_definitions(
    most_steps: usize,
    places: &[&str],
    most_waits: u32,
) -> Vec<ToolDefinition> {
    let mut definitions = typed_definitions(most_steps);
    if let Some(properties) = definitions
        .iter_mut()
        .find(|definition| definition.name == PLAN)
        .and_then(|plan| plan.input_schema.get_mut("properties"))
        .and_then(Value::as_object_mut)
    {
        properties.insert(WAIT_FOR.into(), wait_for_schema(places, most_waits));
    }
    definitions
}

#[cfg(test)]
mod tests {
    use super::super::{Revealed, read_plan};
    use super::*;

    const STEPS: usize = 4;

    /// A plan's `wait_for` is read with its seconds kept within the bound,
    /// its words trimmed, and absent or null as no wait; a plan of only a
    /// wait is an answer, and a wait without seconds is refused.
    #[test]
    fn a_plan_reads_its_wait_within_the_bound() {
        let mut revealed = Revealed::new();
        revealed.reveal("a#b");
        let only = json!({"steps": [], "why": "", "then": "",
            "wait_for": {"app": " Chat ", "contains": "  code ", "seconds": 0}});
        let answer = read_plan(&only, &revealed, STEPS).expect("a wait is something to do");
        assert!(answer.actions.is_empty());
        assert_eq!(
            answer.wait,
            Some(Wait {
                app: Some("Chat".into()),
                contains: Some("code".into()),
                seconds: 1
            })
        );
        let long = json!({"steps": [{"operation": "a#b", "arguments": {}}], "why": "",
            "then": "note the code", "wait_for": {"seconds": 600, "contains": ""}});
        let answer = read_plan(&long, &revealed, STEPS).expect("read");
        assert_eq!(
            answer.wait,
            Some(Wait {
                app: None,
                contains: None,
                seconds: MOST_WAIT_SECONDS
            })
        );
        assert_eq!(answer.then.as_deref(), Some("note the code"));
        let mut plan = json!({"steps": [{"operation": "a#b", "arguments": {}}], "why": "",
            "then": ""});
        assert_eq!(
            read_plan(&plan, &revealed, STEPS).expect("absent").wait,
            None
        );
        plan[WAIT_FOR] = Value::Null;
        assert_eq!(read_plan(&plan, &revealed, STEPS).expect("null").wait, None);
        let untimed = json!({"steps": [], "why": "", "then": "", "wait_for": {}});
        let refused = read_plan(&untimed, &revealed, STEPS).expect_err("no seconds");
        assert!(refused.contains("seconds"), "{refused}");
        let worded = json!({"steps": [], "why": "", "then": "", "wait_for": "a minute"});
        assert!(read_plan(&worded, &revealed, STEPS).is_err());
        let numbered = json!({"steps": [], "why": "", "then": "",
            "wait_for": {"app": 3, "seconds": 5}});
        assert!(read_plan(&numbered, &revealed, STEPS).is_err());
    }

    /// Only a waiting planner's `plan` takes `wait_for`; its places are
    /// named in words, never as an enum, and the set stays the same bytes
    /// on every call for the prompt cache.
    #[test]
    fn the_waiting_plan_names_its_places_in_words_and_stays_byte_stable() {
        let places = ["mail", "chat", "messages"];
        let waiting = typed_waiting_definitions(STEPS, &places, 4);
        assert_eq!(
            serde_json::to_string(&waiting).unwrap(),
            serde_json::to_string(&typed_waiting_definitions(STEPS, &places, 4)).unwrap()
        );
        let plan = waiting.iter().find(|tool| tool.name == PLAN).unwrap();
        let wait = &plan.input_schema["properties"][WAIT_FOR];
        assert_eq!(
            wait["properties"]["app"]["description"],
            "Where it arrives: mail, chat, messages; omit for any."
        );
        assert_eq!(wait["properties"]["seconds"]["maximum"], MOST_WAIT_SECONDS);
        assert!(
            wait["description"]
                .as_str()
                .is_some_and(|said| said.contains("may wait 4 times")),
            "{wait}"
        );
        assert!(
            !plan.input_schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!(WAIT_FOR)),
            "a plan need not wait"
        );
        assert!(
            !serde_json::to_string(&waiting)
                .unwrap()
                .contains("\"enum\"")
        );
        let unwaiting = typed_definitions(STEPS);
        assert!(
            unwaiting[3].input_schema["properties"]
                .get(WAIT_FOR)
                .is_none(),
            "a planner that cannot wait never offers it"
        );
        assert_eq!(
            waiting.iter().map(|tool| &tool.name).collect::<Vec<_>>(),
            unwaiting.iter().map(|tool| &tool.name).collect::<Vec<_>>()
        );
    }
}
