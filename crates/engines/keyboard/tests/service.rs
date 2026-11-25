//! The keyboard's engine, as its view asks it: each op of `engine.call` answered for the app the
//! keys are drawn on, and refused for any other.

use serde_json::Value;

/// The app the keyboard asks as.
const APP: &str = "so.tilo.osk";

fn ask(op: &str, input: Value) -> Value {
    tilo_keyboard_engine::register();
    let answer = tilolang_tilo::engine::answer(APP, op, &input.to_string())
        .unwrap_or_else(|why| panic!("`{op}` is answered: {why}"));
    serde_json::from_str(&answer).expect("an answer is JSON")
}

#[test]
fn the_keyboard_asks_its_own_engine_and_no_other() {
    tilo_keyboard_engine::register();
    assert!(
        tilolang_tilo::engine::answer("so.tilo.notes", "layouts", "{}").is_err(),
        "an app's engine answers that app alone"
    );
    assert!(
        tilolang_tilo::engine::answer(APP, "no-such-op", "{}").is_err(),
        "an op the keyboard does not make is refused"
    );
}

/// The layouts the keyboard may draw: every purpose picks one, and each is keys with a label, an
/// action and a width.
#[test]
fn the_layouts_are_answers_with_keys_and_the_purpose_each_purpose_starts() {
    let answer = ask("layouts", serde_json::json!({}));
    let layouts = answer["layouts"].as_array().expect("layouts");
    let qwerty = layouts
        .iter()
        .find(|layout| layout["name"] == "qwerty")
        .expect("qwerty");
    assert_eq!(qwerty["rows"].as_array().expect("rows").len(), 4);
    assert_eq!(qwerty["rows"][0].as_array().expect("a row").len(), 10);
    assert_eq!(
        answer["purposes"]["telephone"], "phone",
        "a phone field opens the dialpad"
    );
    assert_eq!(answer["purposes"]["password"], "qwerty");
}

/// A swipe over the letters the app draws decodes words, the caller having judged the path a
/// swipe itself: the path is in the same units as the key band it crossed.
#[test]
fn a_swipe_over_the_drawn_letters_decodes_words() {
    let rows = [
        "q:1,w:1,e:1,r:1,t:1,y:1,u:1,i:1,o:1,p:1",
        "a:1,s:1,d:1,f:1,g:1,h:1,j:1,k:1,l:1",
        "Shift:1.5,z:1,x:1,c:1,v:1,b:1,n:1,m:1,Backspace:1.5",
    ];
    // The key band a keyboard of the shipped theme's rows fills on a 720×360 surface, and a
    // dense path through the centres of q, u, i, c, k in it.
    let area = [8.0, 0.0, 712.0, 192.0];
    let layout = tilo_keyboard_engine::layout::from_rows(&rows.map(str::to_owned)).expect("rows");
    let centre = |letter: char| {
        tilo_keyboard_engine::key_rects(
            &layout,
            vello::kurbo::Rect::new(area[0], area[1], area[2], area[3]),
            tilo_keyboard_engine::ShiftState::Off,
        )
        .into_iter()
        .find(|(id, _)| {
            layout
                .key(*id)
                .is_some_and(|key| key.label == letter.to_string())
        })
        .map(|(_, rect)| rect.center())
        .unwrap_or_else(|| panic!("{letter} is a key"))
    };
    let mut path: Vec<(f64, f64)> = Vec::new();
    for letter in "quick".chars() {
        let at = centre(letter);
        if let Some(previous) = path.last().copied() {
            for step in 1..=5 {
                let t = f64::from(step) / 5.0;
                path.push((
                    previous.0 + (at.x - previous.0) * t,
                    previous.1 + (at.y - previous.1) * t,
                ));
            }
        } else {
            path.push((at.x, at.y));
        }
    }
    let answer = ask(
        "swipe",
        serde_json::json!({
            "path": path.iter().flat_map(|(x, y)| [*x, *y]).collect::<Vec<f64>>(),
            "area": area,
            "rows": rows,
            "judged": true,
        }),
    );
    assert_eq!(answer["swipe"], true);
    let candidates = answer["candidates"].as_array().expect("candidates");
    assert!(
        !candidates.is_empty(),
        "a path over the keys decodes: {answer}"
    );
    assert_eq!(
        candidates[0]["word"], "quick",
        "the path through q, u, i, c, k is that word: {answer}"
    );
}

/// A tap is the key the finger meant, answered with the positions it taught; a secret field
/// teaches nothing.
#[test]
fn a_tap_answers_the_key_meant_and_only_a_field_that_may_learn_teaches() {
    let rows = [
        "q:1,w:1,e:1,r:1,t:1,y:1,u:1,i:1,o:1,p:1",
        "a:1,s:1,d:1,f:1,g:1,h:1,j:1,k:1,l:1",
        "Shift:1.5,z:1,x:1,c:1,v:1,b:1,n:1,m:1,Backspace:1.5",
    ];
    let at = serde_json::json!({
        "point": [0.05, 0.125],
        "area": [0.0, 0.0, 1.0, 1.0],
        "rows": rows,
        "language": "en",
        "personalization": true,
    });
    let answer = ask("touch", at.clone());
    assert_eq!(
        answer["key"],
        serde_json::json!([0, 0]),
        "a tap in the middle of the first key is that key: {answer}"
    );
    assert!(
        answer["taught"].is_object(),
        "a field that may learn is taught where the finger landed: {answer}"
    );
    let secret = ask(
        "touch",
        serde_json::json!({
            "point": [0.05, 0.125],
            "area": [0.0, 0.0, 1.0, 1.0],
            "rows": rows,
            "language": "en",
            "personalization": false,
        }),
    );
    assert!(secret["taught"].is_null(), "a secret teaches nothing");
}

/// A word's correction and its neighbours, and what committing it teaches the next word.
#[test]
fn a_word_is_corrected_by_the_keys_it_was_typed_on_and_what_it_teaches_is_answered() {
    let answer = ask(
        "correct",
        serde_json::json!({ "word": "quikc", "language": "en", "layout": "qwerty" }),
    );
    assert_eq!(
        answer["correction"], "quick",
        "two letters swapped are corrected: {answer}"
    );
    let kept = ask(
        "correct",
        serde_json::json!({
            "word": "quikc",
            "language": "en",
            "layout": "qwerty",
            "kept": ["quikc"],
        }),
    );
    assert!(
        kept["correction"].is_null(),
        "a word the person kept is never corrected again: {kept}"
    );
    let learned = ask(
        "learn",
        serde_json::json!({ "word": "you", "previous": "hi" }),
    );
    assert_eq!(
        learned["transition"],
        serde_json::json!(["hi", "you"]),
        "a word after another is one transition: {learned}"
    );
}

/// What a long press offers on each letter: the letter first, then its forms the typing language
/// uses first, cased for the shift.
#[test]
fn a_long_press_offers_the_letters_forms_cased_for_the_shift() {
    let answer = ask(
        "accents",
        serde_json::json!({ "keys": ["a", "e", "q"], "language": "en" }),
    );
    let a = answer["forms"]["a"].as_array().expect("a's forms");
    assert_eq!(a[0], "a", "the letter itself comes first");
    assert!(a.iter().any(|form| form == "à"), "a offers à: {answer}");
    assert!(
        answer["forms"]["q"]
            .as_array()
            .expect("q's forms")
            .is_empty(),
        "a letter with no forms offers none, so no long press opens on it"
    );
    let upper = ask(
        "accents",
        serde_json::json!({ "keys": ["a"], "language": "en", "shift": "shift" }),
    );
    assert_eq!(upper["forms"]["a"][0], "A");
    assert!(
        upper["forms"]["a"]
            .as_array()
            .expect("a's forms")
            .iter()
            .any(|form| form == "À"),
        "the forms are cased too: {upper}"
    );
}

/// A word that names an emoji offers it.
#[test]
fn a_word_that_names_an_emoji_offers_it() {
    let answer = ask("emoji", serde_json::json!({ "word": "pizza", "limit": 2 }));
    let emoji = answer["emoji"].as_array().expect("emoji");
    assert!(!emoji.is_empty(), "pizza names one: {answer}");
}
