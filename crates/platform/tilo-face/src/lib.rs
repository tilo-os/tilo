//! A person's face before any picture: their initials and the presence hue
//! their identity derives. Every surface that draws a person (the toolkit's
//! avatars, the login screen, the boot sign-in screen before the system
//! volume is open) takes both from here, so one person looks the same on
//! each.

/// How many presence hues there are.
pub const PRESENCE_LEN: usize = 12;

/// The first letters of the first and the last word, upper-cased:
/// `"Ada Lovelace"` gives `"AL"`, `"Grace Brewster Hopper"` gives `"GH"`,
/// `"ada"` gives `"A"`. Words without a letter are skipped, so `"Tilo 4"`
/// gives `"T"`, never `"T4"`.
#[must_use]
pub fn initials(name: &str) -> String {
    let letters: Vec<char> = name
        .split_whitespace()
        .filter_map(|word| word.chars().find(|c| c.is_alphabetic()))
        .collect();
    let picked: Vec<char> = match letters.as_slice() {
        [] => Vec::new(),
        [only] => vec![*only],
        [first, .., last] => vec![*first, *last],
    };
    picked.into_iter().flat_map(char::to_uppercase).collect()
}

/// The presence slot, `0..PRESENCE_LEN`, someone wears until they choose a
/// colour: a pure function of their identity, so every host paints them the
/// same without asking. The fixture names `me`, `alice`, `bob` and `charlie`
/// are pinned (blue, pink, orange, green) so demos and tests stay
/// recognisable.
#[must_use]
pub fn presence_slot(identity: &str) -> usize {
    match identity {
        "me" => 8,
        "alice" => 11,
        "bob" => 1,
        "charlie" => 4,
        _ => {
            // FNV-1a 64-bit: cheap, deterministic, no RNG state.
            let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
            for byte in identity.as_bytes() {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(0x0100_0000_01b3);
            }
            let slot = hash.checked_rem(PRESENCE_LEN as u64).unwrap_or(0);
            usize::try_from(slot).unwrap_or(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_take_the_first_and_last_word() {
        assert_eq!(initials("Ada Lovelace"), "AL");
        assert_eq!(initials("  grace   brewster hopper "), "GH");
        assert_eq!(initials("(Ops) team"), "OT");
        assert_eq!(initials("Tilo 4"), "T");
        assert_eq!(initials("ada"), "A");
        assert_eq!(initials(""), "");
    }

    #[test]
    fn a_slot_is_stable_and_in_range() {
        assert_eq!(presence_slot("ada"), presence_slot("ada"));
        assert_eq!(presence_slot("alice"), 11);
        for id in ["ada", "quinten", "", "someone@example.org"] {
            assert!(presence_slot(id) < PRESENCE_LEN);
        }
    }
}
