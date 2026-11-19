use super::*;

tilo_test_env::scenario! {
    fn a_control_is_named_in_words_a_person_reads;

    phase names_read_in_sentence_case;
    {
        assert_eq!(sentence("new_note"), "New note");
        assert_eq!(sentence("NewNote"), "New note");
        assert_eq!(sentence("toggle_pin"), "Toggle pin");
        assert_eq!(sentence("Note"), "Note");
        assert_eq!(sentence(""), "");
    }

    phase a_bound_control_is_not_named_by_a_name_in_the_code;
    // A control bound to view state is not announced by that state's name in the code, which
    // a person never says; it is announced by what the view says it is for.
    {
        let state = Binding {
            target: BindTarget::State {
                key: crate::runtime::StateKey::view("w", MemberId::person("me").into(), "draft"),
                name: "draft".to_owned(),
                ty: "Text".to_owned(),
            },
            author: crate::runtime::ModuleHash(String::new()),
        };
        assert_eq!(
            announced(&state, |_, _| "should not be asked".to_owned()),
            ""
        );
        let field = Binding {
            target: BindTarget::Field {
                target: ResourceRef::parse_id("r1", String::new()).expect("a resource id"),
                field: "due".to_owned(),
            },
            author: crate::runtime::ModuleHash(String::new()),
        };
        assert_eq!(announced(&field, |_, field| sentence(field)), "Due");
    }
}

#[test]
fn a_splice_lands_on_character_boundaries() {
    assert_eq!(spliced("Q3 plan", 7, 0, "s"), "Q3 plans");
    assert_eq!(spliced("Q3 plans", 2, 6, ""), "Q3");
    assert_eq!(
        spliced("naïve", 3, 1, "i"),
        "nai\u{308}ve".replace("i\u{308}", "i")
    );
    assert_eq!(spliced("", 9, 9, "x"), "x");
}
