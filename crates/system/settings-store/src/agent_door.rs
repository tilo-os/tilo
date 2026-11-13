//! This device's agent door, as the person set it (`agent_door`): whether an agent on another of
//! their devices is let in without asking here. Kept on this device only, and off unless the person
//! turns it on.

use serde_json::{Map, Value};

pub const SECTION: &str = "agent_door";

pub const ACCEPT_OWN_DEVICES: &str = "accept_own_devices";

/// Whether `document` lets an agent from another of the person's devices in
/// without asking: off unless set.
#[must_use]
pub fn accepts_own_devices(document: &Map<String, Value>) -> bool {
    crate::bool_at(document, SECTION, ACCEPT_OWN_DEVICES).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_from_other_devices_are_asked_about_unless_the_person_said_otherwise() {
        assert!(!accepts_own_devices(&Map::new()));
        let set = |value: Value| {
            let mut document = Map::new();
            document.insert(
                SECTION.into(),
                serde_json::json!({ ACCEPT_OWN_DEVICES: value }),
            );
            accepts_own_devices(&document)
        };
        assert!(set(Value::Bool(true)));
        assert!(!set(Value::Bool(false)));
        assert!(!set(Value::String("true".into())), "only a switch reads");
    }
}
