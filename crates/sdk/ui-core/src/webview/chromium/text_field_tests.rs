use super::super::{command_within_limits, encode_command, validate_event};
use super::{IME_COMPOSITION_LIMIT, text_field_event};
use crate::webview::WebViewCommand;
use serde_json::json;

#[test]
fn compositions_reach_the_host_in_utf16_and_its_field_reports_place_them() {
    // The caret after "日本" is byte 6 of "日本語" and its UTF-16 unit 2.
    assert_eq!(
        encode_command(WebViewCommand::ImeComposition {
            text: "日本語".into(),
            cursor: Some((6, 6)),
        })
        .unwrap(),
        json!({"op":"ime_composition","text":"日本語","selection_start":2,"selection_end":2})
    );
    // No caret given: it stands after the text. An emoji is two units.
    assert_eq!(
        encode_command(WebViewCommand::ImeComposition {
            text: "a😀".into(),
            cursor: None,
        })
        .unwrap()["selection_start"],
        3
    );
    // An offset inside a character counts from that character's start.
    assert_eq!(
        encode_command(WebViewCommand::ImeComposition {
            text: "日本".into(),
            cursor: Some((4, 99)),
        })
        .unwrap()["selection_end"],
        2
    );
    assert_eq!(
        encode_command(WebViewCommand::ImeCommit("日本語".into())).unwrap(),
        json!({"op":"ime_commit","text":"日本語"})
    );
    assert_eq!(
        encode_command(WebViewCommand::ImeCancel).unwrap(),
        json!({"op":"ime_cancel"})
    );
    assert_eq!(
        encode_command(WebViewCommand::ImeFinish).unwrap(),
        json!({"op":"ime_finish"})
    );
    assert!(!command_within_limits(&WebViewCommand::ImeCommit(
        String::new()
    )));
    assert!(!command_within_limits(&WebViewCommand::ImeComposition {
        text: "x".repeat(IME_COMPOSITION_LIMIT + 1),
        cursor: None,
    }));

    let field = json!({"event":"text_field","editable":true,"purpose":"email",
        "multiline":false,"x":20,"y":40,"width":200,"height":30});
    assert!(validate_event(&field).is_ok());
    let reported = text_field_event("text_field", &field, None).unwrap();
    assert_eq!(reported.purpose, crate::ime::InputPurpose::Email);
    assert_eq!(
        reported.bounds,
        Some(vello::kurbo::Rect::new(20.0, 40.0, 220.0, 70.0))
    );
    assert_eq!(reported.composition, None);
    let bounds = json!({"event":"ime_composition_bounds","x":60,"y":44,"width":32,"height":22});
    assert!(validate_event(&bounds).is_ok());
    let composing = text_field_event("ime_composition_bounds", &bounds, Some(reported)).unwrap();
    assert_eq!(
        composing.composition,
        Some(vello::kurbo::Rect::new(60.0, 44.0, 92.0, 66.0))
    );
    // The field reported again where it moved keeps its composition; the
    // composition ending clears it.
    assert!(
        text_field_event("text_field", &field, Some(composing))
            .unwrap()
            .composition
            .is_some()
    );
    assert_eq!(
        text_field_event(
            "ime_composition_bounds",
            &json!({"event":"ime_composition_bounds"}),
            Some(composing)
        )
        .unwrap()
        .composition,
        None
    );
    // Focus leaving the field, and bounds without a field, leave none.
    let left = json!({"event":"text_field","editable":false});
    assert!(validate_event(&left).is_ok());
    assert_eq!(text_field_event("text_field", &left, Some(composing)), None);
    assert_eq!(
        text_field_event("ime_composition_bounds", &bounds, None),
        None
    );
    for malformed in [
        json!({"event":"text_field"}),
        json!({"event":"text_field","editable":true,"x":1,"y":1,"width":1}),
        json!({"event":"text_field","editable":true,"x":1,"y":1,"width":1,"height":1e9}),
        json!({"event":"ime_composition_bounds","x":"1","y":1,"width":1,"height":1}),
    ] {
        assert!(validate_event(&malformed).is_err(), "{malformed}");
    }
}

#[test]
fn keys_carry_their_place_on_the_keyboard() {
    // A Russian layout's Ctrl+С carries the key it is on, which the host
    // turns into the page's Ctrl+C.
    let cmd = encode_command(WebViewCommand::Key {
        key: crate::webview::KeyInput::Character("с".into()),
        down: true,
        modifiers: crate::webview::KeyModifiers {
            ctrl: true,
            ..crate::webview::KeyModifiers::default()
        },
        code: Some("KeyC"),
    })
    .unwrap();
    assert_eq!(cmd["code"], "KeyC");
    assert!(command_within_limits(&WebViewCommand::Key {
        key: crate::webview::KeyInput::Named(crate::webview::NamedKey::Enter),
        down: true,
        modifiers: crate::webview::KeyModifiers::default(),
        code: Some("Space"),
    }));
    assert!(!command_within_limits(&WebViewCommand::Key {
        key: crate::webview::KeyInput::Character("a".into()),
        down: true,
        modifiers: crate::webview::KeyModifiers::default(),
        code: Some("NotAKey"),
    }));
}
