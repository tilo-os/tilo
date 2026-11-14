//! This device's agent door, as the person set it (`agent_door`): whether an agent on another of
//! their devices is let in without asking here, and whether, once in, it takes its steps without
//! asking too. Kept on this device only, and off unless the person turns it on.

use serde_json::{Map, Value};

pub const SECTION: &str = "agent_door";

pub const ACCEPT_OWN_DEVICES: &str = "accept_own_devices";

pub const ACT_WITHOUT_ASKING: &str = "act_without_asking";

/// Whether `document` lets an agent from another of the person's devices in
/// without asking: off unless set.
#[must_use]
pub fn accepts_own_devices(document: &Map<String, Value>) -> bool {
    crate::bool_at(document, SECTION, ACCEPT_OWN_DEVICES).unwrap_or(false)
}

/// Whether `document` lets an agent from another of the person's devices take a step it would
/// ask about here without asking: off unless set, and only while such agents are let in at all.
#[must_use]
pub fn acts_without_asking(document: &Map<String, Value>) -> bool {
    accepts_own_devices(document)
        && crate::bool_at(document, SECTION, ACT_WITHOUT_ASKING).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(section: Value) -> Map<String, Value> {
        let mut document = Map::new();
        document.insert(SECTION.into(), section);
        document
    }

    #[test]
    fn agents_from_other_devices_are_asked_about_unless_the_person_said_otherwise() {
        assert!(!accepts_own_devices(&Map::new()));
        let set = |value: Value| {
            accepts_own_devices(&document(serde_json::json!({ ACCEPT_OWN_DEVICES: value })))
        };
        assert!(set(Value::Bool(true)));
        assert!(!set(Value::Bool(false)));
        assert!(!set(Value::String("true".into())), "only a switch reads");
    }

    #[test]
    fn their_steps_go_unasked_only_while_both_switches_are_on() {
        assert!(!acts_without_asking(&Map::new()));
        let set = |accept: Value, act: Value| {
            acts_without_asking(&document(
                serde_json::json!({ ACCEPT_OWN_DEVICES: accept, ACT_WITHOUT_ASKING: act }),
            ))
        };
        assert!(set(Value::Bool(true), Value::Bool(true)));
        assert!(!set(Value::Bool(true), Value::Bool(false)));
        assert!(
            !set(Value::Bool(false), Value::Bool(true)),
            "an agent not let in without asking takes no step without asking"
        );
        assert!(
            !set(Value::Bool(true), Value::String("true".into())),
            "only a switch reads"
        );
        assert!(!acts_without_asking(&document(
            serde_json::json!({ ACT_WITHOUT_ASKING: true })
        )));
    }
}
