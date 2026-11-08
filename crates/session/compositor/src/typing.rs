//! Which keys type a character on a keymap.
//!
//! A Wayland client turns the keycodes it receives into text with the keymap
//! the seat sent it, so a key event the compositor makes up for a character
//! (test control's `type` and `key`) has to name the key, and the modifiers,
//! that type that character on that keymap: `y` is evdev 21 on a US keymap
//! and evdev 44 on a German one, `@` is Shift+2 on one and `AltGr`+Q on the
//! other. [`stroke_for`] asks the keymap itself, the way a person finds the
//! key on their keyboard. A character no key types on the keymap (`€` on a
//! US keymap) has no stroke; the caller delivers it as text instead.

use smithay::input::keyboard::{Keysym, xkb};

/// The keys that type one character: hold `modifiers` in order, then press and release `key` .
/// Codes are evdev (the kernel's, as `wl_keyboard.key` carries them minus 8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stroke {
    pub modifiers: Vec<StrokeModifier>,
    pub key: u32,
}

/// A modifier key a [`Stroke`] holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StrokeModifier {
    pub level: Level,
    /// The evdev code of the key that sets it on this keymap.
    pub key: u32,
}

/// The shift levels a stroke can need, in the order a person reaches for them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Shift,
    /// The third level: `AltGr` on most European layouts.
    Level3,
}

/// The modifier combinations tried, plainest first: a character a key types unmodified is never
/// typed with Shift or `AltGr` held.
const COMBINATIONS: [&[Level]; 4] = [
    &[],
    &[Level::Shift],
    &[Level::Level3],
    &[Level::Shift, Level::Level3],
];

/// The stroke that types `ch` on group `layout` of `keymap`, or `None` when
/// no key does. `carries(evdev)` says whether a key can be delivered at all;
/// keys it refuses are not used, for the character or as its modifier.
pub fn stroke_for(
    keymap: &xkb::Keymap,
    layout: u32,
    ch: char,
    carries: impl Fn(u32) -> bool,
) -> Option<Stroke> {
    let wanted = xkb::utf32_to_keysym(u32::from(ch));
    if wanted.raw() == 0 {
        return None;
    }
    let keys = || {
        (keymap.min_keycode().raw()..=keymap.max_keycode().raw())
            .filter(|raw| *raw >= 8 && carries(raw.saturating_sub(8)))
            .map(xkb::Keycode::new)
    };
    let modifier_key = |level: Level| {
        let sym = match level {
            Level::Shift => xkb::keysyms::KEY_Shift_L,
            Level::Level3 => xkb::keysyms::KEY_ISO_Level3_Shift,
        };
        let sets_level = || {
            keys().filter(move |key| {
                keymap
                    .key_get_syms_by_level(*key, layout, 0)
                    .contains(&Keysym::new(sym))
            })
        };
        // A keymap also sets a level from keys most keyboards do not have: its extra level key
        // (`<LVL3>`, the key a Japanese keyboard's zenkaku key covers) carries `AltGr` on every
        // layout, ahead of the right alt in keycode order. A stroke holds the modifier a person
        // presses, so that comes first; the extra key is the fallback for a keymap that puts the
        // level nowhere else, so every character stays typeable.
        sets_level()
            .find(|key| crate::sticky_keys::is_modifier(key.raw().saturating_sub(8)))
            .or_else(|| sets_level().next())
    };
    for combination in COMBINATIONS {
        let Some(modifiers) = combination
            .iter()
            .map(|level| modifier_key(*level).map(|key| (*level, key)))
            .collect::<Option<Vec<_>>>()
        else {
            continue;
        };
        let mut state = xkb::State::new(keymap);
        let _ = state.update_mask(0, 0, 0, 0, 0, layout);
        for (_, key) in &modifiers {
            let _ = state.update_key(*key, xkb::KeyDirection::Down);
        }
        let held: Vec<xkb::Keycode> = modifiers.iter().map(|(_, key)| *key).collect();
        if let Some(key) = keys()
            .filter(|key| !held.contains(key))
            .find(|key| state.key_get_one_sym(*key) == wanted)
        {
            return Some(Stroke {
                modifiers: modifiers
                    .into_iter()
                    .map(|(level, key)| StrokeModifier {
                        level,
                        key: key.raw().saturating_sub(8),
                    })
                    .collect(),
                key: key.raw().saturating_sub(8),
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keymap(layout: &str) -> xkb::Keymap {
        // A keymap of this test's own: the machine running the test may set `XKB_DEFAULT_LAYOUT` ,
        // `XKB_DEFAULT_VARIANT` or `XKB_DEFAULT_OPTIONS` (its own layout, and what its desktop asks
        // for), and its xkb configuration says what an empty name means. None of that may reach
        // these keymaps, so every name below is stated and the context reads none from the
        // environment.
        let context = xkb::Context::new(xkb::CONTEXT_NO_ENVIRONMENT_NAMES);
        xkb::Keymap::new_from_names(
            &context,
            "evdev",
            "pc105",
            layout,
            "",
            Some(String::new()),
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .expect("the XKB data has the layout")
    }

    const SHIFT: StrokeModifier = StrokeModifier {
        level: Level::Shift,
        key: 42,
    };
    const ALTGR: StrokeModifier = StrokeModifier {
        level: Level::Level3,
        key: 100,
    };

    fn stroke(layout: &xkb::Keymap, ch: char) -> Option<(Vec<StrokeModifier>, u32)> {
        stroke_for(layout, 0, ch, |code| code < 256).map(|stroke| (stroke.modifiers, stroke.key))
    }

    #[test]
    fn a_character_is_the_key_its_keymap_puts_it_on() {
        let us = keymap("us");
        let de = keymap("de");
        assert_eq!(stroke(&us, 'y'), Some((vec![], 21)));
        assert_eq!(stroke(&us, 'z'), Some((vec![], 44)));
        assert_eq!(stroke(&de, 'y'), Some((vec![], 44)), "German swaps Y and Z");
        assert_eq!(stroke(&de, 'z'), Some((vec![], 21)));
        assert_eq!(stroke(&us, 'Y'), Some((vec![SHIFT], 21)));
        assert_eq!(stroke(&us, '@'), Some((vec![SHIFT], 3)));
        assert_eq!(stroke(&de, '@'), Some((vec![ALTGR], 16)), "AltGr+Q");
        assert_eq!(stroke(&de, '€'), Some((vec![ALTGR], 18)), "AltGr+E");
        assert_eq!(stroke(&de, 'ä'), Some((vec![], 40)));
        assert_eq!(stroke(&de, 'Ö'), Some((vec![SHIFT], 39)));
        assert_eq!(stroke(&us, ' '), Some((vec![], 57)));
    }

    #[test]
    fn a_character_no_key_types_has_no_stroke() {
        let us = keymap("us");
        assert_eq!(stroke(&us, 'ä'), None);
        assert_eq!(stroke(&us, '€'), None);
        assert_eq!(stroke(&us, '😀'), None);
    }

    #[test]
    fn a_stroke_holds_a_modifier_a_person_presses() {
        // A broadcast keymap also sets the levels from its extra keys, which no keyboard but an old
        // Japanese one carries; a stroke must hold a modifier key instead.
        for layout in ["us", "de", "fr", "us,de"] {
            let keymap = keymap(layout);
            for ch in ['@', '€', 'Ö', 'Ä', 'ü'] {
                let Some(stroke) = stroke_for(&keymap, 0, ch, |code| (1..=255).contains(&code))
                else {
                    continue;
                };
                assert!(
                    stroke
                        .modifiers
                        .iter()
                        .all(|modifier| crate::sticky_keys::is_modifier(modifier.key)),
                    "{layout} {ch}: {stroke:?}"
                );
            }
        }
    }

    #[test]
    fn a_key_that_cannot_be_delivered_is_not_used() {
        let de = keymap("de");
        let stroke = stroke_for(&de, 0, '@', |code| code != 100 && code < 256);
        assert!(
            stroke
                .as_ref()
                .is_none_or(|stroke| stroke.modifiers.iter().all(|modifier| modifier.key != 100)),
            "{stroke:?}"
        );
        // Without the right alt the keymap's extra level key still holds the level, so `@` stays
        // typeable rather than becoming text.
        assert_eq!(stroke.map(|stroke| stroke.key), Some(16), "@");
    }

    #[test]
    fn the_active_group_decides() {
        let both = keymap("us,de");
        assert_eq!(
            stroke_for(&both, 1, 'y', |code| code < 256).map(|stroke| stroke.key),
            Some(44)
        );
        assert_eq!(
            stroke_for(&both, 0, 'y', |code| code < 256).map(|stroke| stroke.key),
            Some(21)
        );
    }
}
