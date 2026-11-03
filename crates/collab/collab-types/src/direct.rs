//! A record two people share privately: `direct/<pair id>`.
//!
//! A conversation between two people is an ordinary record of the workspace,
//! one record = one channel = one content-key group, read by the two people's
//! devices and by no one else's
//! ([`member_record`](crate::member_record) is the same shape for one person).
//! Its id names the pair rather than the text, so anyone in the workspace can
//! derive it: that a conversation exists is metadata, its text is not.
//!
//! The pair id is the one the owner-side conversation store has always spelt
//! (`direct_` and the hash of the workspace and the two people, both keyed
//! and sorted), so a stored conversation moves to its record unchanged.

const RECORD_PREFIX: &str = "direct/";

/// Domain of [`direct_pair_id`].
const PAIR_DOMAIN: &str = "private-person-pair-v1";

fn canonical(key: &str) -> bool {
    key.len() == 64
        && key
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The id of the pair `a` and `b` share in `workspace`: `direct_` and the
/// hash of the workspace, the two people and [`PAIR_DOMAIN`]. `None` for a
/// workspace that is not a UUID, and for a key that is not canonical
/// lowercase hex.
#[must_use]
pub fn direct_pair_id(workspace: &str, a: &str, b: &str) -> Option<String> {
    let workspace = uuid::Uuid::parse_str(workspace).ok()?;
    if !canonical(a) || !canonical(b) || a == b {
        return None;
    }
    let (low, high) = if a < b { (a, b) } else { (b, a) };
    let bytes = serde_json::to_vec(&(PAIR_DOMAIN, workspace, [low, high])).ok()?;
    Some(format!("direct_{}", blake3::hash(&bytes).to_hex()))
}

/// The record carrying the conversation of `pair` ([`direct_pair_id`]).
#[must_use]
pub fn direct_record(pair: &str) -> String {
    format!("{RECORD_PREFIX}{pair}")
}

/// The pair whose record `record` is, `None` for any other record.
#[must_use]
pub fn direct_pair(record: &str) -> Option<&str> {
    record.strip_prefix(RECORD_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACE: &str = "0b5c3f1e-7d2a-4c1b-9e8f-6a5d4c3b2a19";
    const ADA: &str = "11";
    const BOB: &str = "22";

    fn person(byte: u8) -> String {
        hex::encode([byte; 32])
    }

    #[test]
    fn a_pairs_record_names_the_pair_in_either_order() {
        let (ada, bob) = (person(0x11), person(0x22));
        let pair = direct_pair_id(WORKSPACE, &ada, &bob).unwrap();
        assert_eq!(direct_pair_id(WORKSPACE, &bob, &ada), Some(pair.clone()));
        assert_eq!(direct_pair(&direct_record(&pair)), Some(pair.as_str()));
        assert_eq!(direct_pair("tab:1"), None);
    }

    #[test]
    fn the_stored_pair_spelling_is_the_records() {
        // The spelling the owner-side conversation store keeps, from the same
        // workspace and people.
        let (ada, bob) = (person(0x11), person(0x22));
        let mut people = [ada.as_str(), bob.as_str()];
        people.sort_unstable();
        let stored = serde_json::to_vec(&(
            "private-person-pair-v1",
            uuid::Uuid::parse_str(WORKSPACE).unwrap(),
            people,
        ))
        .unwrap();
        assert_eq!(
            direct_pair_id(WORKSPACE, &ada, &bob),
            Some(format!("direct_{}", blake3::hash(&stored).to_hex()))
        );
    }

    #[test]
    fn only_two_distinct_people_in_a_workspace_name_a_pair() {
        assert_eq!(direct_pair_id("not-a-workspace", ADA, BOB), None);
        assert_eq!(
            direct_pair_id(WORKSPACE, ADA, BOB),
            None,
            "keys are canonical lowercase hex"
        );
        let ada = person(0x11);
        assert_eq!(direct_pair_id(WORKSPACE, &ada, &ada), None);
    }
}
