//! What an action makes its calls from, and the items a key names: `act(args).made_from(r)` in
//! an action, `made_from`, and `insert(item, key: k)`.

use crate::common::{codes, compile_body, validate};

const BOARD: &str = "type Note {\n    title: Text\n}\ntype Board {\n    notes: Collection<Note>\n}\n";

tilo_test_env::scenario! {
    /// An action calls another made from what it made, and inserts the item a key names.
    fn an_action_makes_its_calls_from_what_it_made;

    phase a_call_in_an_action_is_made_from_a_resource;
    {
        let c = compile_body(&format!(
            "{BOARD}\
action answer(b: Board) -> Note {{\n    if let from = made_from {{\n        b.notes.insert(Note(title: \"re\"), key: from.id())\n    }} else {{\n        b.notes.insert(Note(title: \"re\"))\n    }}\n}}\n\
action add(b: Board) -> Note {{\n    let n = b.notes.insert(Note(title: \"a\"))\n    answer(b).made_from(n)\n    n\n}}"
        ));
        validate(&c.wasm);
        let has = |name: &str| c.wasm.windows(name.len()).any(|w| w == name.as_bytes());
        assert!(has("act_made_from"), "the call carries what it is made from");
        assert!(has("coll_insert_keyed"), "the keyed insert names its item by the key");
    }

    phase only_a_call_of_an_action_is_made_from_something;
    {
        assert_eq!(
            codes(&format!(
                "{BOARD}fn title(n: Note) -> Text {{\n    n.title\n}}\n\
action add(b: Board) -> Note {{\n    let n = b.notes.insert(Note(title: \"a\"))\n    let t = title(n).made_from(n)\n    n\n}}"
            )),
            ["E022"]
        );
    }

    phase only_an_action_reads_what_it_is_made_from;
    {
        assert_eq!(codes("fn f() -> Bool {\n    made_from == .none\n}"), ["E049"]);
    }

    phase a_key_is_text;
    {
        assert_eq!(
            codes(&format!(
                "{BOARD}action add(b: Board) -> Note {{\n    b.notes.insert(Note(title: \"a\"), key: 3)\n}}"
            )),
            ["E020"]
        );
    }
}
