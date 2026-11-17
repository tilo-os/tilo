//! A `tilo` call's reply: what the person reads once its program has run.

/// An app's declarations reach the first turn compacted: no docs, a type on one line, each
/// action as declared.
#[test]
fn declarations_are_compacted_for_the_first_turn() {
    let described = "app so.tilo.lang.tasks \"Tasks\"\n\n/// Something to do.\ntype Task {\n    \
                     /// What.\n    title: Text\n    due: Option<Date>\n}\n\nenum Priority {\n    \
                     none\n    high\n}\n\n/// Adds a task.\nprimary action new_task(tasks: Tasks, \
                     due: Option<Date>, title: Text) -> Task\n";
    assert_eq!(
        super::compact_declarations(described),
        "type Task { title: Text, due: Option<Date> }\nenum Priority { none, high }\nprimary \
         action new_task(tasks: Tasks, due: Option<Date>, title: Text) -> Task\n"
    );
}

/// What a program printed stands in its reply; the API's bookkeeping about the step does not.
#[test]
fn a_reply_takes_what_the_program_printed() {
    let answer = "Added \"Call mum\" for Tue 17 Nov at 18:00\nstep 1: 1 change in tasks; \
                  `undo` takes it back";
    assert_eq!(
        super::reply_with_answer(super::ANSWER_SLOT, answer),
        "Added \"Call mum\" for Tue 17 Nov at 18:00"
    );
    assert_eq!(
        super::reply_with_answer("Done.", answer),
        "Done.",
        "a reply without the slot stands as written"
    );
}
