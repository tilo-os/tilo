//! What is being written and has not been sent: a resource of Chat's beside
//! the conversation, kept in the workspace Chat is open in. It is
//! [`chat_model::ChatDrafts`], which workspace roles authorize members to read or change.

pub use chat_model::drafts::{ChatDrafts, ChatDraftsAction, drafts, line};

pub(crate) use ui::tilo_actions::policy::roles::WORKSPACE_CHAT_DRAFTS_DOCUMENT as DOCUMENT;
use ui::tilo_resource;
pub(crate) type DraftRecord = ui::framework::WorkspaceResource<ChatDrafts>;

#[derive(Clone, tilo_resource::Effect)]
#[effect(operation = "chat.previous-drafts")]
pub(crate) enum ImportEffect {
    /// Import earlier channel drafts.
    #[effect(
        kind = "undoable",
        attended,
        privileged,
        returns,
        label = "chat-import-previous-drafts"
    )]
    ImportPreviousDrafts,
}

pub(crate) fn import_previous(
    opener: &ui::framework::ResourceOpener<ChatDrafts>,
) -> Result<usize, String> {
    use ui::tilo_resource::Id;
    let record = opener
        .open(DOCUMENT)
        .filter(|record| record.may_read() && record.may("import-previous"))
        .ok_or("the workspace must grant permission to import channel drafts")?;
    let Some((source, previous, _)) = opener.legacy_member_content()? else {
        return Ok(0);
    };
    let current = record.get();
    let mut imported = 0;
    for (key, draft) in &previous.drafts {
        let channel = key
            .split_once("::t::")
            .map_or(key.as_str(), |(channel, _)| channel);
        if crate::chat::Chat::is_direct(&Id::from_raw(channel))
            || current.drafts.contains_key(key)
            || draft.text().trim().is_empty()
        {
            continue;
        }
        let receipt = format!("{source}/{key}");
        if current.imported.contains(&receipt) {
            continue;
        }
        record
            .invoke(&ChatDrafts::IMPORT_PREVIOUS.with((receipt, key.clone(), draft.markdown())))
            .map_err(|error| error.to_string())?;
        imported += 1;
    }
    Ok(imported)
}

#[cfg(test)]
mod tests {
    use ui::tilo_resource::{Invoke, Replica};

    use super::*;

    tilo_test_env::scenario! {
        /// A person's drafts: edited apart, and found by search.
        /// Each case was a test of its own, on a fixture of its own; a failure
        /// names the cases that failed.
        fn a_draft_is_edited_at_its_key_and_found_by_its_words;

        phase a_draft_is_rich_text_edited_at_its_key;
        {
            let mut ada = Replica::<ChatDrafts>::new().as_author("u_ada");
            let append = ChatDrafts::paths()
                .drafts
                .key("c_general")
                .perform(&tilo_document::RichTextAction::AppendParagraph {
                    text: "half a thought".into(),
                })
                .unwrap();
            ada.apply(Invoke::edit(&append).unwrap()).unwrap();
            assert_eq!(
                crate::projection::draft_text(ada.doc(), "c_general").trim_start(),
                "half a thought"
            );
            // Typing into a draft never started starts it, holding just what
            // was typed; the other drafts stay as they were.
            let elsewhere = ChatDrafts::paths()
                .drafts
                .key("c_random")
                .perform(&tilo_document::RichTextAction::AppendParagraph {
                    text: "an aside".into(),
                })
                .unwrap();
            ada.apply(Invoke::edit(&elsewhere).unwrap()).unwrap();
            assert_eq!(
                crate::projection::draft_text(ada.doc(), "c_random").trim_start(),
                "an aside"
            );
            assert_eq!(
                crate::projection::draft_text(ada.doc(), "c_general").trim_start(),
                "half a thought"
            );
            assert_eq!(ada.state().unwrap().drafts.len(), 2);
        }

        phase the_words_not_yet_sent_are_found;
        // A draft is found by the words that were typed, opening the conversation it is written in.
        {
            let mut ada = Replica::<ChatDrafts>::new().as_author("u_ada");
            let append = ChatDrafts::paths()
                .drafts
                .key("c_general")
                .perform(&tilo_document::RichTextAction::AppendParagraph {
                    text: "see you at 8".into(),
                })
                .unwrap();
            ada.apply(Invoke::edit(&append).unwrap()).unwrap();

            let entries = ada.live().expect("the draft reads").search();
            assert_eq!(entries.len(), 1, "one draft, one entry: {entries:?}");
            assert!(
                entries[0].title.ends_with("see you at 8"),
                "{}",
                entries[0].title
            );
            assert_eq!(
                entries[0].anchor.as_deref(),
                Some("chat.workspace/channels/c_general"),
                "the conversation it is written in"
            );
        }
    }
}
