//! The row the person moved to stays theirs when a late answer re-ranks the list.
use super::tests::{arrow_down, data, selected, settled, with_a_late_app};

tilo_test_env::scenario! {
    fn the_row_the_person_saw_or_moved_to_stays_theirs_when_an_answer_arrives;

    phase an_answer_that_arrives_late_keeps_the_row_the_person_moved_to;
    // The person arrowed to a row; an app's answer arriving a frame later
    // re-ranks the list. Enter must still open the row they moved to, not
    // whatever slid under the cursor.
    {
        let (mut search, arrived) = with_a_late_app(data());
        search.set_query("notes");
        settled(&mut search);
        assert!(search.waiting(), "the app's answer is still coming");
        assert!(search.results().len() >= 2, "{:?}", search.results());
        assert!(arrow_down(&mut search));
        let chosen = selected(&search).expect("a row is selected");

        arrived.set(true);
        assert!(search.refresh(), "the answer re-ranks the rows");
        settled(&mut search);
        assert!(
            search
                .results()
                .iter()
                .any(|hit| hit.title == "notes from the meeting"),
            "the late answer is listed: {:?}",
            search.results()
        );
        assert_eq!(selected(&search), Some(chosen));
    }
}
