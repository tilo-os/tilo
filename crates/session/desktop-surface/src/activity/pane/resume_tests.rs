//! The resume card and the tasks a run left: what a person picks up again, and what a run linked.
//! Each is its own module, as the pane's other test areas are.

use super::*;

mod resume_card_tests {
    use super::*;
    use ui::tilo_actions::ActionContext;
    use ui::widget::history::HistoryAuthor;

    fn resume_entry(id: u8, author: &str, timestamp: SystemTime) -> HistoryEntry {
        HistoryEntry {
            id: HistoryEntryId([id; 16]),
            author: HistoryAuthor::new(author, 210.0, ui::history_source::short_id(author)),
            entry_kind: EntryKind::Text,
            artifact_kind: ArtifactKind::File,
            timestamp,
            summary: "Edited assumptions".into(),
            text_delta: Some((4, 1)),
            diff_payload: None,
            redaction: None,
            context: Some(
                ActionContext::for_resource("finance:q2", "sheet", "Q2 finance report")
                    .with_app("so.tilo.sheets", "Sheets"),
            ),
        }
    }

    #[test]
    fn resume_uses_the_local_users_latest_targeted_action() {
        let local = "kai";
        let now = SystemTime::now();
        let older = now.checked_sub(Duration::from_secs(60)).unwrap_or(now);
        let entries = vec![resume_entry(1, local, older), resume_entry(2, "Ada", now)];
        let resume =
            local_resume_snapshot(&entries, local).expect("local targeted work is resumable");
        assert_eq!(resume.target.label, "Q2 finance report");
        assert_eq!(resume.timestamp, older, "a peer's newer work is not yours");
        assert!(
            local_resume_snapshot(&entries, "").is_none(),
            "an unknown local identity resumes nothing"
        );
    }
}
