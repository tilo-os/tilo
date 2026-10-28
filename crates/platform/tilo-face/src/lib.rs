//! A person's face before any picture: their initials and the presence hue
//! their identity derives, and how a colour reads as a presence hue
//! ([`decode_srgb`], [`linear_to_oklab`]). Every surface that draws a person (the toolkit's
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

/// An sRGB channel as linear light.
#[must_use]
pub fn decode_srgb(v: u8) -> f64 {
    let s = f64::from(v) / 255.0;
    if s <= 0.04045 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear sRGB in `OKLab` (Björn Ottosson's matrices): how a colour reads as
/// a presence hue, the same on every surface (the toolkit's
/// `ui::color::Oklch::from_color` and `PresenceHue::nearest_to_srgb`).
#[must_use]
#[allow(
    clippy::many_single_char_names,
    reason = "l/a/b and l/m/s are the names the OKLab paper gives these axes"
)]
pub fn linear_to_oklab(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
    let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
    let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
    (
        0.210_454_255_3 * l + 0.793_617_785_0 * m - 0.004_072_046_8 * s,
        1.977_998_495_1 * l - 2.428_592_205_0 * m + 0.450_593_709_9 * s,
        0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766_0 * s,
    )
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
        for id in ["ada", "ada", "", "someone@example.org"] {
            assert!(presence_slot(id) < PRESENCE_LEN);
        }
    }
}
