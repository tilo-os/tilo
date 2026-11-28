//! What the person shares of what they are doing, and what the people in each workspace see of
//! them: the switches their own card offers, kept with them so every device of theirs reads the
//! same choice.

use std::collections::BTreeMap;

use crate::Key;

pub const SECTION: &str = "presence";

/// How what they are doing is shared: `automatic` (their device may write it for them),
/// `manual` (only what they write) or `off` (nothing).
pub const SHARING: Key<String> = Key::new(SECTION, "sharing", || "automatic".to_owned());

/// What the people in each workspace see of them, by the workspace's id: `where` (their face,
/// their window and their pointer), `here` (their face alone) or `nothing`. A workspace they never
/// chose for reads as `where`.
pub const VISIBILITY: Key<BTreeMap<String, String>> =
    Key::new(SECTION, "visibility", BTreeMap::<String, String>::new);
