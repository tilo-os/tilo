//! The id of a conversation's record: `direct/<pair id>`.
//!
//! A conversation between two people is an ordinary record of the workspace,
//! decided by the workspace's policy like any other record. Its id names the
//! pair rather than the text, so either person's device derives the same id
//! and both reach the one record they share.
//!
//! The pair id is the one the owner-side conversation store has always spelt
//! (`direct_` and the hash of the workspace and the two people, both keyed
//! and sorted), so a stored conversation is imported into its record unchanged
//! and a record written before it stays readable.

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

/// Whether `pair` is spelt as [`direct_pair_id`] spells one: `direct_` and
/// the hash, lowercase hex.
#[must_use]
pub fn canonical_pair(pair: &str) -> bool {
    pair.strip_prefix("direct_").is_some_and(canonical)
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
        assert_eq!(direct_record(&pair), format!("direct/{pair}"));
        assert!(canonical_pair(&pair));
        assert!(!canonical_pair("direct/not-a-pair"));
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
