use crate::{Catalogue, Gender, Recents, SkinTone};

fn cat() -> &'static Catalogue {
    Catalogue::english()
}

fn texts(found: &[&crate::Emoji]) -> Vec<&'static str> {
    found.iter().map(|e| e.text()).collect()
}

#[test]
fn loads_every_group_in_unicode_order() {
    let cat = cat();
    let ids: Vec<_> = cat.groups().iter().map(crate::Group::id).collect();
    assert_eq!(
        ids,
        [
            "smileys-emotion",
            "people-body",
            "animals-nature",
            "food-drink",
            "travel-places",
            "activities",
            "objects",
            "symbols",
            "flags",
        ]
    );
    assert_eq!(cat.groups()[0].label(), "Smileys & Emotion");
    // Every tab icon is a real catalogue entry.
    assert!(cat.groups().iter().all(|g| cat.get(g.icon()).is_some()));
    assert!(cat.emoji().len() > 1800, "{}", cat.emoji().len());
    let first = cat.group_emoji(&cat.groups()[0]);
    assert_eq!(first[0].text(), "😀");
    assert_eq!(first[0].name(), "grinning face");
    let sub = &cat.subgroups(&cat.groups()[0])[0];
    assert_eq!(sub.id(), "face-smiling");
    assert_eq!(cat.subgroup_emoji(sub)[0].text(), "😀");
    // Every entry is toneless: toned forms come from with_skin_tone.
    assert!(cat.emoji().iter().all(|e| {
        e.text()
            .chars()
            .all(|c| SkinTone::from_modifier(c).is_none())
    }));
}

#[test]
fn thumbs_finds_thumbs_up_first() {
    let found = cat().search("thumbs", 5);
    assert_eq!(texts(&found)[..2], ["👍", "👎"]);
}

#[test]
fn ranking_prefers_prefix_then_word_then_substring() {
    let cat = cat();
    // "cat" is all of 🐈's name but only one word of 😸's.
    let found = texts(&cat.search("cat", 50));
    let pos = |e: &str| found.iter().position(|f| *f == e).unwrap();
    assert!(pos("🐈") < pos("😸"), "{found:?}");
    assert_eq!(cat.search("pizza", 1)[0].text(), "🍕");
    // A whole word beats the start of a longer name, and shorter names lead.
    assert_eq!(cat.search("heart", 1)[0].text(), "❤\u{FE0F}");
    // Keywords count: "+1" is a thumbs-up keyword.
    assert_eq!(cat.search("+1", 1)[0].text(), "👍");
    // Multi-term queries need every term.
    assert_eq!(cat.search("thumbs down", 1)[0].text(), "👎");
    assert!(cat.search("zzzqqq", 5).is_empty());
    assert!(cat.search("  ", 5).is_empty());
    // Diacritics fold.
    assert!(texts(&cat.search("cote d'ivoire", 3)).contains(&"🇨🇮"));
}

#[test]
fn skin_tone_on_pizza_is_a_noop() {
    let cat = cat();
    assert!(!cat.get("🍕").unwrap().takes_skin_tone());
    for tone in SkinTone::ALL {
        assert_eq!(cat.with_skin_tone("🍕", Some(tone)), Some("🍕"));
    }
}

#[test]
fn thumbs_up_medium_tone_is_the_modifier_sequence() {
    let cat = cat();
    let toned = cat.with_skin_tone("👍", Some(SkinTone::Medium)).unwrap();
    assert_eq!(toned, "\u{1F44D}\u{1F3FD}");
    // Re-toning and un-toning a toned emoji.
    assert_eq!(
        cat.with_skin_tone(toned, Some(SkinTone::Dark)),
        Some("\u{1F44D}\u{1F3FF}")
    );
    assert_eq!(cat.with_skin_tone(toned, None), Some("👍"));
    let found = cat.lookup(toned).unwrap();
    assert_eq!(found.emoji.text(), "👍");
    assert_eq!(found.tone, Some(SkinTone::Medium));
    assert_eq!(cat.with_skin_tone("not an emoji", None), None);
}

#[test]
fn tone_replaces_presentation_selector_and_covers_zwj_people() {
    let cat = cat();
    // ☝️ is U+261D U+FE0F; its toned form drops the selector.
    assert_eq!(
        cat.with_skin_tone("☝️", Some(SkinTone::Light)),
        Some("\u{261D}\u{1F3FB}")
    );
    // A multi-person sequence takes one uniform tone on every person, not on
    // the handshake between them.
    assert_eq!(cat.with_skin_tone("🧑‍🤝‍🧑", Some(SkinTone::Medium)), Some("🧑🏽‍🤝‍🧑🏽"));
    assert_eq!(cat.with_skin_tone("👩‍❤️‍👨", Some(SkinTone::Dark)), Some("👩🏿‍❤️‍👨🏿"));
    // A profession: tone lands on the person, not the tool.
    assert_eq!(
        cat.with_skin_tone("🧑‍⚕️", Some(SkinTone::MediumDark)),
        Some("🧑🏾‍⚕️")
    );
    // Every toned form resolves back to its base.
    for e in cat.emoji().iter().filter(|e| e.takes_skin_tone()) {
        for tone in SkinTone::ALL {
            let toned = cat.with_skin_tone(e.text(), Some(tone)).unwrap();
            assert_ne!(toned, e.text());
            let found = cat.lookup(toned).unwrap();
            assert_eq!((found.emoji.text(), found.tone), (e.text(), Some(tone)));
        }
    }
}

#[test]
fn gender_variants_map_person_man_woman() {
    let cat = cat();
    let v = cat.gender_variants("🏃").unwrap();
    assert_eq!(v.neutral.text(), "🏃");
    assert_eq!(v.man.unwrap().text(), "🏃‍♂️");
    assert_eq!(v.woman.unwrap().text(), "🏃‍♀️");
    assert_eq!(cat.get("🏃‍♀️").unwrap().gender(), Some(Gender::Woman));
    // Resolves from any member, and keeps the skin tone.
    assert_eq!(cat.gender_variants("👩‍⚕️").unwrap().neutral.text(), "🧑‍⚕️");
    assert_eq!(cat.with_gender("🧑🏽‍⚕️", Gender::Woman), Some("👩🏽‍⚕️"));
    assert!(cat.gender_variants("🍕").is_none());
}

#[test]
fn suggests_emoji_for_words() {
    let cat = cat();
    assert_eq!(cat.suggest("pizza", 3)[0].text(), "🍕");
    assert_eq!(cat.suggest("Pizzas", 3)[0].text(), "🍕");
    assert!(!cat.suggest("cat", 3).is_empty());
    assert!(cat.suggest("a", 3).is_empty());
    assert!(cat.suggest("thumbs up", 3).is_empty());
}

#[test]
fn locale_falls_back_to_english() {
    assert_eq!(Catalogue::for_locale("en-GB").locale(), "en");
    assert_eq!(Catalogue::for_locale("xx").locale(), "en");
    assert_eq!(Catalogue::for_locale("").locale(), "en");
    assert_eq!(
        Catalogue::for_locale("pt-BR").search("thumbs", 1)[0].text(),
        "👍"
    );
    assert!(Catalogue::locales().any(|l| l == "en"));
}

#[test]
fn locale_tags_resolve_to_their_language() {
    assert_eq!(
        Catalogue::locales().collect::<Vec<_>>(),
        ["en", "de", "es", "fr", "it", "ja", "zh", "zh_Hant"]
    );
    for (tag, locale) in [
        ("de-DE", "de"),
        ("de_AT.UTF-8", "de"),
        ("FR", "fr"),
        ("es-419", "es"),
        ("it_IT@euro", "it"),
        ("ja-JP", "ja"),
        ("zh-Hans-CN", "zh"),
        ("zh-CN", "zh"),
        ("zh-TW", "zh_Hant"),
        ("zh_HK.UTF-8", "zh_Hant"),
        ("zh-Hant", "zh_Hant"),
        ("zh-Hans-HK", "zh"),
    ] {
        assert_eq!(Catalogue::for_locale(tag).locale(), locale, "{tag}");
    }
    // One catalogue per language, built once.
    assert!(std::ptr::eq(
        Catalogue::for_locale("de-DE"),
        Catalogue::for_locale("de-CH")
    ));
}

#[test]
fn translated_names_and_search() {
    let de = Catalogue::for_locale("de-DE");
    assert_eq!(de.get("👍").unwrap().name(), "Daumen hoch");
    assert_eq!(de.search("daumen", 1)[0].text(), "👍");
    assert_eq!(de.search("Daumen runter", 1)[0].text(), "👎");
    assert_eq!(de.groups()[8].label(), "Flaggen");
    assert_eq!(de.suggest("pizza", 1)[0].text(), "🍕");

    let fr = Catalogue::for_locale("fr-FR");
    assert_eq!(fr.search("pouce vers le haut", 1)[0].text(), "👍");
    // "cœur" folds to "coeur".
    assert_eq!(fr.search("coeur rouge", 1)[0].text(), "❤\u{FE0F}");
    for (locale, query) in [
        ("fr", "pouce"),
        ("es", "pulgar"),
        ("it", "pollice"),
        ("ja", "親指"),
        ("zh", "拇指"),
    ] {
        let found = texts(&Catalogue::for_locale(locale).search(query, 5));
        assert!(found.contains(&"👍"), "{locale} {query}: {found:?}");
    }

    assert_eq!(
        Catalogue::for_locale("zh-TW").get("👍").unwrap().name(),
        "讚"
    );
    // Structure is shared: tones and genders work the same in every locale.
    assert_eq!(de.with_skin_tone("👍", Some(SkinTone::Dark)), Some("👍🏿"));
    assert_eq!(de.emoji().len(), cat().emoji().len());
}

#[test]
fn untranslated_entries_keep_their_english_names() {
    // CLDR 46 names Sark's flag in English only.
    let english = cat().get("🇨🇶").unwrap().name();
    assert_eq!(english, "flag: Sark");
    for locale in Catalogue::locales() {
        let cat = Catalogue::for_locale(locale);
        assert_eq!(cat.get("🇨🇶").unwrap().name(), english, "{locale}");
        // Every emoji and group has a real name in every locale.
        assert!(cat.emoji().iter().all(|e| e.name() != e.text()), "{locale}");
        assert!(cat.groups().iter().all(|g| g.label() != g.id()), "{locale}");
    }
    assert_eq!(
        Catalogue::for_locale("de").search("sark", 1)[0].text(),
        "🇨🇶"
    );
    // Italian CLDR inherits the smileys label, so it stays English.
    assert_eq!(
        Catalogue::for_locale("it").groups()[0].label(),
        "Smileys & Emotion"
    );
}

#[test]
fn recents_are_most_recent_first_deduped_and_capped() {
    let mut r = Recents::new(3);
    for e in ["👍", "🍕", "🎉", "🍕"] {
        r.record(e);
    }
    assert_eq!(r.items(), ["🍕", "🎉", "👍"]);
    r.record("🚀");
    assert_eq!(r.items(), ["🚀", "🍕", "🎉"]);
    // A new skin tone replaces the old spelling rather than duplicating it.
    r.record("🎉");
    r.record("👍🏽");
    r.record("👍🏿");
    assert_eq!(r.items(), ["👍🏿", "🎉", "🚀"]);
    r.record("  ");
    assert_eq!(r.len(), 3);
    r.forget("👍");
    assert_eq!(r.items(), ["🎉", "🚀"]);
}

#[test]
fn recents_round_trip_and_renormalise() {
    let mut r = Recents::new(2);
    r.record("👍");
    r.record("🍕");
    let json = serde_json::to_string(&r).unwrap();
    assert_eq!(serde_json::from_str::<Recents>(&json).unwrap(), r);
    let merged: Recents =
        serde_json::from_str(r#"{"capacity":2,"items":["🍕","👍","🍕","🎉"]}"#).unwrap();
    assert_eq!(merged.items(), ["🍕", "👍"]);
}

#[test]
fn translated_search_also_matches_english_after_native_matches() {
    let de = Catalogue::for_locale("de-DE");
    // English still finds emoji, shown by their German names.
    let found = de.search("thumbs", 5);
    assert_eq!(texts(&found)[..2], ["👍", "👎"]);
    assert_eq!(found[0].name(), "Daumen hoch");
    assert_eq!(de.search("thumbs up", 1)[0].text(), "👍");
    // An emoji matching in both languages is listed once.
    let pizza = texts(&de.search("pizza", 50));
    assert_eq!(pizza.iter().filter(|e| **e == "🍕").count(), 1);
    // Every native match ranks before every English-only one. A term only
    // matches text that contains it, so containment tells them apart.
    // Search folds umlauts, so "Händchen" matches "hand" too.
    let folded = |s: &str| s.to_lowercase().replace('ä', "a");
    let native = |e: &crate::Emoji| {
        folded(e.name()).contains("hand") || e.keywords().any(|k| folded(k).contains("hand"))
    };
    let found = de.search("hand", 500);
    let first_english = found.iter().position(|e| !native(e)).unwrap();
    assert!(first_english > 0, "German has native \"hand\" matches");
    assert!(found[first_english..].iter().all(|e| !native(e)));
    // The English catalogue has no second pass.
    assert!(cat().search("daumen", 5).is_empty());
}
