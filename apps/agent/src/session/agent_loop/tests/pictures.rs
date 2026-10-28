use super::super::pictures::{MOST_PICTURE_BYTES, picture_type};
use super::*;

/// A picture an invoke answered with is shown to the model after the
/// turn's results, as an image of its type; what is not a picture, is
/// not on this device or is too large is not.
#[test]
fn pictures_invokes_answered_with_are_shown_to_the_model() {
    use base64::Engine as _;
    let fixture = fixture("Look at the canvas", World::default(), Vec::new());
    let png = [b"\x89PNG\r\n\x1a\n".as_slice(), b"pixels"].concat();
    let content = &fixture.session.cfg.content;
    let address = |bytes: &[u8]| format!("tilo.content:{}", content.put(bytes).unwrap());
    let picture = address(&png);
    let text = address(b"not a picture");
    let large = address(&[png.as_slice(), &vec![0; MOST_PICTURE_BYTES]].concat());
    let missing = format!("tilo.content:{}", "0".repeat(64));
    let mut state = LoopState::new();
    state.pictures = vec![picture.clone(), text, large, missing];
    let mut conversation = Conversation::neutral("system", "look");
    fixture.session.show_pictures(&mut conversation, &mut state);
    assert!(state.pictures.is_empty());
    let body = conversation.body(&[]);
    let messages = body["messages"].as_array().unwrap();
    let shown = messages.last().unwrap();
    assert_eq!(shown["role"], "user");
    let shown = shown.to_string();
    assert!(shown.contains(&format!("in order: {picture}.")), "{shown}");
    assert!(shown.contains("image/png"), "{shown}");
    assert!(shown.contains(&base64::engine::general_purpose::STANDARD.encode(&png)));
    assert_eq!(shown.matches("image/").count(), 1, "{shown}");

    // Nothing to show adds no turn.
    let before = conversation.body(&[]);
    fixture.session.show_pictures(&mut conversation, &mut state);
    assert_eq!(conversation.body(&[]), before);
    assert_eq!(picture_type(b"GIF89a.."), Some("image/gif"));
    assert_eq!(picture_type(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
    assert_eq!(picture_type(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
}
