use super::*;
use settings_store::personal_dictionary::{Edit, Service, Vocabulary, Word};

fn type_word(osk: &mut OskWidget, word: &str) {
    for c in word.chars() {
        osk.apply_action(KeyAction::Char(c.to_string()));
    }
}

#[test]
fn shared_additions_reach_osk_and_bias_reader_without_forcing_replacements() {
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(root.path().to_owned()).unwrap();
    let reader = service.bind(Some([1; 32])).unwrap();
    let dictation = reader.clone();
    let mut osk = OskWidget::new();
    osk.set_personal_vocabulary(Some(std::sync::Arc::new(reader.clone())));
    osk.set_personalization_enabled(false);
    service
        .edit(&reader, 0, Edit::Add(Word::new("Tilo", None).unwrap()))
        .unwrap();
    type_word(&mut osk, "ti");
    assert_eq!(osk.suggestions().first().unwrap().word, "Tilo");
    assert_eq!(
        dictation
            .snapshot()
            .unwrap()
            .hints("en")
            .collect::<Vec<_>>(),
        ["Tilo"]
    );
    assert!(osk.activate_suggestion(0));
    assert_eq!(osk.pending_text(), "Tilo ");
    osk.reset_composition();
    type_word(&mut osk, "tile");
    osk.apply_action(KeyAction::Space);
    assert_eq!(osk.pending_text(), "tile ");
    osk.reset_composition();
    assert_eq!(osk.correction_for("teh"), Some("the".into()));
    service
        .edit(&reader, 1, Edit::Add(Word::new("teh", None).unwrap()))
        .unwrap();
    assert!(osk.correction_for("teh").is_none());
    type_word(&mut osk, "teh");
    osk.apply_action(KeyAction::Space);
    assert_eq!(osk.pending_text(), "teh ");
}

#[test]
fn stale_personal_chips_cannot_insert_removed_edited_or_another_accounts_words() {
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(root.path().to_owned()).unwrap();
    let reader = service.bind(Some([1; 32])).unwrap();
    let word = Word::new("Tilo", None).unwrap();
    service.edit(&reader, 0, Edit::Add(word.clone())).unwrap();
    let mut osk = OskWidget::new();
    osk.set_personal_vocabulary(Some(std::sync::Arc::new(reader.clone())));
    type_word(&mut osk, "ti");
    service
        .edit(
            &reader,
            1,
            Edit::Replace {
                old: word.clone(),
                new: Word::new("TiLo", None).unwrap(),
            },
        )
        .unwrap();
    assert!(!osk.activate_suggestion(0));
    assert_eq!(osk.suggestions().first().unwrap().word, "TiLo");
    service.edit(&reader, 2, Edit::Remove(word)).unwrap();
    assert!(!osk.activate_suggestion(0));
    assert!(!osk.suggestions().iter().any(|s| s.personal));
    service
        .edit(&reader, 3, Edit::Add(Word::new("Tilo", None).unwrap()))
        .unwrap();
    osk.refresh_personal_vocabulary();
    assert!(osk.suggestions().first().unwrap().personal);
    service.bind(Some([2; 32]));
    assert!(!osk.activate_suggestion(0));
    assert!(!osk.suggestions().iter().any(|s| s.personal));
    osk.set_personal_vocabulary(None);
    assert!(!osk.suggestions().iter().any(|s| s.personal));
}

#[test]
fn language_specific_unicode_hints_and_secret_fields_preserve_input_intent() {
    let root = tempfile::tempdir().unwrap();
    let service = Service::new(root.path().to_owned()).unwrap();
    let reader = service.bind(Some([1; 32])).unwrap();
    service
        .edit(
            &reader,
            0,
            Edit::Add(Word::new("Élodie", Some("fr")).unwrap()),
        )
        .unwrap();
    let mut osk = OskWidget::new();
    osk.set_personal_vocabulary(Some(std::sync::Arc::new(reader)));
    type_word(&mut osk, "él");
    assert!(!osk.suggestions().iter().any(|s| s.personal));
    osk.reset_composition();
    assert!(osk.select_text_layout("azerty"));
    type_word(&mut osk, "él");
    assert_eq!(osk.suggestions().first().unwrap().word, "Élodie");
    assert!(osk.activate_suggestion(0));
    assert_eq!(osk.pending_text(), "Élodie ");
    osk.reset_composition();
    osk.set_purpose(InputPurpose::Password);
    type_word(&mut osk, "él");
    assert!(osk.suggestions().is_empty());
}
