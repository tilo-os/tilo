use tilo_ui_base::keycaps::*;
use std::collections::BTreeMap;
use tilo_ui_base::interaction::KeyboardFamily;
use tilo_ui_base::input::KeyboardModifier;
use std::sync::atomic::Ordering;

tilo_test_env::scenario! {
    fn the_active_layout_is_scoped_published_and_typed;

    phase a_scoped_layout_stays_on_its_thread_and_unwinds;
    {
        let german = Keycaps::from_xkb("de", "");
        let outer = scoped(german);
        assert_eq!(active(), german);
        let other = std::thread::spawn(active).join().ok();
        assert_eq!(
            other,
            Some(Keycaps::from_code(ACTIVE.load(Ordering::Relaxed))),
            "another thread keeps reading the process-wide layout"
        );
        {
            let _mac = scoped(Keycaps::from_xkb("us", "mac"));
            assert_eq!(active(), Keycaps::from_xkb("us", "mac"));
        }
        assert_eq!(active(), german, "the inner scope restores the outer");
        drop(outer);
        assert_eq!(active(), Keycaps::from_code(ACTIVE.load(Ordering::Relaxed)));
    }

    phase a_chord_shows_the_keys_the_layout_presses_for_its_mark;
    {
        let _german = scoped(Keycaps::from_xkb("de,us", ",intl"));
        assert_eq!(
            active().language,
            KeycapLanguage::German,
            "the first group types"
        );
        let mark = |key: &str, shift: bool, alt_gr: bool| TypedMark {
            key: key.to_owned(),
            shift,
            alt_gr,
        };
        set_typed_marks(BTreeMap::from([
            ('/', mark("7", true, false)),
            ('?', mark("ß", true, false)),
            ('=', mark("0", true, false)),
            ('-', mark("-", false, false)),
            ('[', mark("8", false, true)),
        ]));
        let typed = |modifiers: &[&str], key: &str| {
            let (held, key) = as_typed(modifiers.iter().copied(), key);
            active().chord(held.iter().map(String::as_str), &key)
        };
        assert_eq!(typed(&["Super"], "/"), "Super+Umschalt+7");
        assert_eq!(typed(&["Super", "Shift"], "/"), "Super+Umschalt+ß");
        assert_eq!(typed(&["Super"], "="), "Super+Umschalt+0");
        assert_eq!(typed(&["Super"], "-"), "Super+-");
        assert_eq!(typed(&["Super"], "["), "Super+Alt Gr+8");
        assert_eq!(typed(&["Super", "Shift"], "s"), "Super+Umschalt+S");
        assert_eq!(typed(&["Super"], "pageup"), "Super+Bild↑");
        assert_eq!(
            Keycaps::US.chord(["Super"], "/"),
            "Super+/",
            "the canonical spelling stays the chord's own"
        );
        set_typed_marks(BTreeMap::new());
        assert_eq!(
            typed(&["Super"], "/"),
            "Super+/",
            "no marks: the chord as written"
        );
    }

    phase toolkit_legends_and_palette_hints_follow_the_active_layout;
    {
        use tilo_ui_base::{input::KeyLegend, input::KeyboardBinding};
        use tilo_ui_base::{platform::Platform, platform::PlatformConventions, platform::Shortcut};
        let linux = PlatformConventions::new(Platform::Linux);
        let binding = KeyboardBinding::new(
            [KeyboardModifier::Control, KeyboardModifier::Shift],
            KeyLegend::Delete,
        );
        let entry = crate::CommandEntry::new("x", "X").keybinding("Super+PageUp");
        set_active(Keycaps::from_xkb("de", ""));
        let german = (
            binding.keycap_labels(linux),
            Shortcut::command("S").shift().label(linux),
            entry.keybinding_label(linux),
        );
        set_active(Keycaps::US);
        assert_eq!(german.0, ["Strg", "Umschalt", "Entf"]);
        assert_eq!(german.1, "Strg+Umschalt+S");
        assert_eq!(german.2.as_deref(), Some("Super+Bild↑"));
        assert_eq!(binding.keycap_labels(linux), ["Ctrl", "Shift", "Delete"]);
    }

    phase the_active_layout_round_trips_every_combination;
    {
        let before = active();
        for language in KeycapLanguage::ALL {
            for family in [KeyboardFamily::Pc, KeyboardFamily::Apple] {
                let keycaps = Keycaps { language, family };
                set_active(keycaps);
                assert_eq!(active(), keycaps);
            }
        }
        set_active(before);
    }
}

fn chord(keycaps: Keycaps, modifiers: &[&str], key: &str) -> String {
    keycaps.chord(modifiers.iter().copied(), key)
}

tilo_test_env::scenario! {
    fn each_layout_writes_chords_in_its_own_legends;

    phase us_is_the_default_and_keeps_the_shell_spelling;
    {
        let us = Keycaps::from_xkb("us", "");
        assert_eq!(us, Keycaps::default());
        assert_eq!(chord(us, &["Shift", "Super"], "s"), "Super+Shift+S");
        assert_eq!(chord(us, &["Ctrl", "Alt"], "escape"), "Ctrl+Alt+Esc");
        assert_eq!(chord(us, &["Super"], "pageup"), "Super+Page Up");
        assert_eq!(chord(us, &[], "printscreen"), "Print Screen");
        assert_eq!(chord(us, &["Alt"], "f4"), "Alt+F4");
        assert_eq!(chord(us, &["Super"], "/"), "Super+/");
        assert_eq!(us.modifier(KeyboardModifier::AltGr), "AltGr");
        assert_eq!(Keycaps::from_xkb("ru", ""), us);
        assert_eq!(Keycaps::from_xkb("gb", "extd"), us);
    }

    phase german_layouts_use_german_legends;
    {
        for (layout, variant) in [("de", ""), ("de", "nodeadkeys"), ("at", "")] {
            let de = Keycaps::from_xkb(layout, variant);
            assert_eq!(de.language, KeycapLanguage::German, "{layout}:{variant}");
            assert_eq!(de.family, KeyboardFamily::Pc);
        }
        let de = Keycaps::from_xkb("de", "");
        assert_eq!(chord(de, &["Ctrl", "Shift"], "t"), "Strg+Umschalt+T");
        assert_eq!(de.modifier(KeyboardModifier::AltGr), "Alt Gr");
        assert_eq!(de.modifier(KeyboardModifier::System), "Super");
        assert_eq!(de.key("delete"), "Entf");
        assert_eq!(de.key("insert"), "Einfg");
        assert_eq!(de.key("home"), "Pos1");
        assert_eq!(de.key("end"), "Ende");
        assert_eq!(chord(de, &["Super"], "pageup"), "Super+Bild↑");
        assert_eq!(chord(de, &["Super"], "pagedown"), "Super+Bild↓");
        assert_eq!(chord(de, &["Super", "Shift"], "left"), "Super+Umschalt+←");
        assert_eq!(chord(de, &["Ctrl", "Alt"], "escape"), "Strg+Alt+Esc");
    }

    phase french_layouts_use_french_legends;
    {
        for layout in ["fr", "be"] {
            assert_eq!(
                Keycaps::from_xkb(layout, "").language,
                KeycapLanguage::French
            );
        }
        let fr = Keycaps::from_xkb("fr", "azerty");
        assert_eq!(chord(fr, &["Ctrl", "Shift"], "a"), "Ctrl+Maj+A");
        assert_eq!(chord(fr, &["Ctrl", "Alt"], "escape"), "Ctrl+Alt+Échap");
        assert_eq!(fr.key("delete"), "Suppr");
        assert_eq!(fr.key("pageup"), "Pg préc");
        assert_eq!(fr.modifier(KeyboardModifier::AltGr), "Alt Gr");
    }

    phase english_legend_layouts_only_rename_alt_gr;
    {
        for (layout, language) in [
            ("nl", KeycapLanguage::Dutch),
            ("pt", KeycapLanguage::Portuguese),
            ("br", KeycapLanguage::Portuguese),
            ("se", KeycapLanguage::Nordic),
            ("no", KeycapLanguage::Nordic),
            ("dk", KeycapLanguage::Nordic),
            ("fi", KeycapLanguage::Nordic),
            ("ch", KeycapLanguage::Swiss),
            ("jp", KeycapLanguage::Japanese),
        ] {
            let keycaps = Keycaps::from_xkb(layout, "");
            assert_eq!(keycaps.language, language, "{layout}");
            assert_eq!(
                chord(keycaps, &["Super", "Ctrl", "Shift"], "pageup"),
                "Super+Ctrl+Shift+Page Up",
                "{layout}"
            );
            assert_eq!(keycaps.key("delete"), "Delete", "{layout}");
            assert_eq!(keycaps.modifier(KeyboardModifier::AltGr), "Alt Gr");
        }
        assert_eq!(Keycaps::from_xkb("ch", "fr"), Keycaps::from_xkb("ch", ""));
    }

    phase mac_variants_use_apple_symbols_written_together;
    {
        for (layout, variant) in [
            ("us", "mac"),
            ("de", "mac"),
            ("ch", "de_mac"),
            ("fr", "mac"),
        ] {
            let mac = Keycaps::from_xkb(layout, variant);
            assert_eq!(mac.family, KeyboardFamily::Apple, "{layout}:{variant}");
            assert_eq!(chord(mac, &["Shift", "Super"], "s"), "⌘⇧S");
            assert_eq!(chord(mac, &["Ctrl", "Alt"], "escape"), "⌃⌥esc");
        }
        let mac = Keycaps::from_xkb("de", "mac_nodeadkeys");
        assert_eq!(mac.language, KeycapLanguage::German);
        assert_eq!(mac.key("pageup"), "⇞");
        assert_eq!(mac.key("left"), "←");
        assert_eq!(mac.key("insert"), "Einfg");
        assert_eq!(
            Keycaps::from_xkb("de", "").on_family(KeyboardFamily::Apple),
            Keycaps {
                language: KeycapLanguage::German,
                family: KeyboardFamily::Apple
            }
        );
    }

    phase native_keypad_and_control_names_use_human_legends;
    {
        let de = Keycaps::from_xkb("de", "");
        for (name, label) in [
            ("KP_Insert", "Einfg"),
            ("KP_Delete", "Entf"),
            ("KP_Home", "Pos1"),
            ("KP_Beg", "Löschen"),
            ("KP_Prior", "Bild↑"),
            ("Pause", "Pause"),
            ("Sys_Req", "S-Abf"),
            ("Scroll_Lock", "Rollen"),
            ("KP_Divide", "÷"),
        ] {
            assert_eq!(de.key(name), label);
        }
        assert_eq!(Keycaps::US.key("Pause"), "Pause");
        assert_eq!(Keycaps::US.key("KP_Delete"), "Delete");
    }

    phase native_language_keys_and_dead_symbols_use_shared_legends;
    {
        let japanese = Keycaps::from_xkb("jp", "");
        assert_eq!(japanese.native_symbol("Muhenkan", ""), "無変換");
        assert_eq!(japanese.native_symbol("Henkan_Mode", ""), "変換");
        assert_eq!(japanese.native_symbol("Hiragana_Katakana", ""), "かな");
        let korean = Keycaps::from_xkb("kr", "");
        assert_eq!(korean.native_symbol("Hangul", ""), "한/영");
        assert_eq!(korean.native_symbol("Hangul_Hanja", ""), "한자");
        assert_eq!(Keycaps::US.native_symbol("dead_acute", ""), "◌́");
        assert_eq!(Keycaps::US.native_symbol("Greek_alpha", "α"), "α");
    }

    phase tokens_and_unknown_keys_pass_through;
    {
        let de = Keycaps::from_xkb("de", "");
        assert_eq!(de.token("Ctrl"), "Strg");
        assert_eq!(de.token("PageUp"), "Bild↑");
        assert_eq!(de.token("K"), "K");
        assert_eq!(de.key("f12"), "F12");
        assert_eq!(de.key("volumeup"), "volumeup");
        assert_eq!(de.key("ß"), "ß");
        assert_eq!(de.key("ö"), "Ö");
    }
}

#[test]
fn spanish_and_italian_layouts_use_their_legends() {
    let es = Keycaps::from_xkb("es", "");
    assert_eq!(
        Keycaps::from_xkb("latam", "").language,
        KeycapLanguage::Spanish
    );
    assert_eq!(chord(es, &["Ctrl", "Shift"], "enter"), "Ctrl+Mayús+Intro");
    assert_eq!(es.key("delete"), "Supr");
    assert_eq!(es.key("pagedown"), "Av Pág");
    let it = Keycaps::from_xkb("it", "");
    assert_eq!(chord(it, &["Ctrl", "Shift"], "delete"), "Ctrl+Maiusc+Canc");
    assert_eq!(it.key("enter"), "Invio");
    assert_eq!(it.key("end"), "Fine");
}
