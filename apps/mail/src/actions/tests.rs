//! The mailbox on its own and as the window publishes it: verbs, field
//! edits, what the servers are told, merging, the published places, and the
//! values the verbs carry.

use super::*;
use crate::app::MailApp;
use crate::model::Folder;
use crate::resource::{
    MAX_DRAFT_BODY_CHARS, MAX_DRAFT_SUBJECT_CHARS, MAX_MESSAGES_PER_ACTION, Mail, MailShell,
    MessageState, OPEN_MESSAGE_LIMIT, folder_resource, mailbox,
};
use crate::sample::sample_mailbox;
use crate::state::{CtxAction, MailState};
use automerge::AutoCommit;
use tilo_actions::ActionRegistry;
use tilo_actions::ActionSpec;
use tilo_document::RichTextAction;
use ui::framework::testing::TestApp;
use ui::tilo_resource::{Edit, Id, Invoke, Replica, Resource as _};

fn invitation(events: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Tilo//Calendar//EN\r\nMETHOD:REQUEST\r\n{events}END:VCALENDAR\r\n"
    )
}

const EVENT: &str = "BEGIN:VEVENT\r\nUID:a@tilo\r\nSUMMARY:Review\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT10M\r\nEND:VALARM\r\nEND:VEVENT\r\n";

#[test]
fn calendar_invitation_acceptance_is_structural_and_honours_overrides() {
    // Phase `calendar_invitation_accepts_a_request_and_its_overrides`.
    {
        assert!(valid_calendar_invitation(&invitation(EVENT)));
        let override_event = "BEGIN:VEVENT\r\nUID:a@tilo\r\nRECURRENCE-ID;VALUE=DATE:20260619\r\nSTATUS:CANCELLED\r\nEND:VEVENT\r\n";
        assert!(
            valid_calendar_invitation(&invitation(&format!("{EVENT}{override_event}"))),
            "an overridden occurrence of the same event is part of one invitation"
        );
        let folded = invitation(EVENT).replace("SUMMARY:Review", "SUMMARY:Rev\r\n iew");
        assert!(valid_calendar_invitation(&folded));
    }

    // Phase `calendar_invitation_is_checked_structurally_not_textually`.
    {
        let lookalike = EVENT.replace("SUMMARY:Review", "SUMMARY:BEGIN:VEVENT");
        assert!(
            valid_calendar_invitation(&invitation(&lookalike)),
            "event text that looks like a component is just text"
        );
        let two_events = format!("{EVENT}{}", EVENT.replace("UID:a@tilo", "UID:b@tilo"));
        assert!(!valid_calendar_invitation(&invitation(&two_events)));
        let method_in_event = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nMETHOD:REQUEST\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        assert!(!valid_calendar_invitation(method_in_event));
        let publish = invitation(EVENT).replace("METHOD:REQUEST", "METHOD:PUBLISH");
        assert!(!valid_calendar_invitation(&publish));
        let no_uid = invitation(&EVENT.replace("UID:a@tilo\r\n", ""));
        assert!(!valid_calendar_invitation(&no_uid));
        let misnested = invitation(&EVENT.replace("END:VALARM", "END:VEVENT"));
        assert!(!valid_calendar_invitation(&misnested));
        let trailing = format!("{}BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n", invitation(EVENT));
        assert!(!valid_calendar_invitation(&trailing));
        assert!(!valid_calendar_invitation(
            &invitation(EVENT).replace("\r\n", "\n")
        ));
        assert!(!valid_calendar_invitation(&invitation("")));
        let oversized = invitation(&format!(
            "BEGIN:VEVENT\r\nUID:a\r\nDESCRIPTION:{}\r\nEND:VEVENT\r\n",
            "x".repeat(MAX_CALENDAR_ICS_BYTES)
        ));
        assert!(!valid_calendar_invitation(&oversized));
    }
}

// --- the mailbox on its own ------------------------------------------------

fn apply(mail: &mut Replica<Mail>, invoke: Invoke<Mail>) -> Result<(), String> {
    mail.apply(invoke)
        .map(drop)
        .map_err(|error| error.to_string())
}

fn edit(mail: &mut Replica<Mail>, edit: &Edit<Mail>) -> Result<(), String> {
    apply(mail, Invoke::edit(edit).map_err(|error| error.to_string())?)
}

fn ingest(mail: &mut Replica<Mail>, messages: Vec<IngestBaseline>) {
    apply(mail, Mail::INGEST.with(mailbox::Ingest { messages })).unwrap();
}

fn register(mail: &mut Replica<Mail>, id: &str, folder: Folder) {
    ingest(
        mail,
        vec![IngestBaseline {
            folder: Some(folder.as_key().into()),
            ..IngestBaseline::register_only(id)
        }],
    );
}

fn state(mail: &Replica<Mail>, id: &str) -> Option<MessageState> {
    mail.state().unwrap().message(id).cloned()
}

fn at(id: &str) -> <MessageState as ui::tilo_resource::Model>::Paths<Mail> {
    Mail::paths().messages.item(&Id::from_raw(id))
}

fn ids(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|id| (*id).to_owned()).collect()
}

/// A message fetched from `account` as its server has it.
fn fetched(id: &str, read: bool, folder: Folder) -> IngestBaseline {
    IngestBaseline {
        id: id.into(),
        read: Some(read),
        starred: Some(false),
        folder: Some(folder.as_key().into()),
        account: Some("you@example.test".into()),
        locator: Some(format!("{id}@example.test")),
    }
}

/// Flags overlay the loaded mail; a delayed fetch never overwrites what the
/// reader did, and a message deleted for good stays deleted.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each phase and its fixture visible"
)]
fn flags_and_purges_converge_without_resurrection() {
    // Phase `flags_persist_and_overlay`.
    {
        let mut mail = Replica::<Mail>::new();
        let mut mb = sample_mailbox();
        let id = *mb.messages.keys().next().unwrap();
        mb.mark_read(id, true);
        mb.toggle_star(id);
        let starred = mb.get(id).unwrap().starred;
        ingest(
            &mut mail,
            vec![IngestBaseline::from_message(mb.get(id).unwrap())],
        );
        let mut fresh = mb.clone();
        for m in fresh.messages.values_mut() {
            m.read = false;
            m.starred = false;
        }
        mail.state().unwrap().apply_flags(&mut fresh);
        assert_eq!(fresh.get(id).unwrap().starred, starred);
        assert!(fresh.get(id).unwrap().read);
    }

    // Phase `delayed_ingestion_never_overwrites_a_reader_change`.
    {
        let mut mail = Replica::<Mail>::new();
        let mut mailbox = sample_mailbox();
        let id = *mailbox.messages.keys().next().expect("sample message");
        let fetched = mailbox.get(id).expect("sample message").clone();
        ingest(&mut mail, vec![IngestBaseline::from_message(&fetched)]);
        apply(
            &mut mail,
            Mail::MARK_READ.with(mailbox::MarkRead {
                messages: vec![id.to_string()],
                read: true,
            }),
        )
        .unwrap();
        let mut stale_fetch = fetched;
        stale_fetch.read = false;
        ingest(&mut mail, vec![IngestBaseline::from_message(&stale_fetch)]);
        assert!(state(&mail, &id.to_string()).unwrap().read);
        mail.state().unwrap().apply_flags(&mut mailbox);
        assert!(mailbox.get(id).is_some_and(|message| message.read));
    }

    // Phase `purge_removes_the_hydrated_copy_and_a_stale_fetch_cannot_bring_it_back`.
    {
        let mut mail = Replica::<Mail>::new();
        let mut mailbox = sample_mailbox();
        let id = *mailbox.messages.keys().next().expect("sample message");
        ingest(
            &mut mail,
            vec![IngestBaseline::from_message(mailbox.get(id).unwrap())],
        );
        edit(&mut mail, &at(&id.to_string()).folder.set(Folder::Trash)).unwrap();
        apply(
            &mut mail,
            Mail::PURGE.with(mailbox::Purge {
                messages: vec![id.to_string()],
            }),
        )
        .unwrap_err();
        mail.apply_confirmed(Mail::PURGE.with(mailbox::Purge {
            messages: vec![id.to_string()],
        }))
        .unwrap();
        mail.state().unwrap().apply_flags(&mut mailbox);
        assert!(
            mailbox.get(id).is_none(),
            "a peer converges by dropping the purged hydrated message"
        );
        ingest(
            &mut mail,
            vec![IngestBaseline::from_message(
                sample_mailbox().messages.values().next().unwrap(),
            )],
        );
        ingest(
            &mut mail,
            vec![IngestBaseline::register_only(id.to_string())],
        );
        assert!(
            state(&mail, &id.to_string()).is_none(),
            "still deleted for good"
        );
    }

    // Phase `field_edits_overlay_flags_folder_and_labels`.
    {
        let mut mail = Replica::<Mail>::new();
        let mut mb = sample_mailbox();
        let some = *mb.messages.keys().next().unwrap();
        {
            let m = mb.get_mut(some).unwrap();
            m.read = false;
            m.starred = false;
            m.folder = Folder::Inbox;
        }
        let id = some.to_string();
        register(&mut mail, &id, Folder::Inbox);
        apply(
            &mut mail,
            Mail::MARK_READ.with(mailbox::MarkRead {
                messages: vec![id.clone()],
                read: true,
            }),
        )
        .unwrap();
        edit(&mut mail, &at(&id).starred.set(true)).unwrap();
        edit(&mut mail, &at(&id).folder.set(Folder::Archive)).unwrap();
        edit(&mut mail, &at(&id).labels.insert(&"work".to_owned())).unwrap();
        assert!(
            edit(&mut mail, &at(&id).read.set(false)).is_err(),
            "read changes through the housekeeping verb"
        );
        mail.state().unwrap().apply_flags(&mut mb);
        let m = mb.get(some).unwrap();
        assert!(m.read && m.starred);
        assert_eq!(m.folder, Folder::Archive);
        assert_eq!(m.labels, vec!["work"]);
    }
}

/// The draft, labels and local folders round-trip and stay bounded.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each phase and its fixture visible"
)]
fn drafts_labels_and_folders_round_trip_and_stay_bounded() {
    // Phase `draft_round_trips_and_discards`.
    {
        let mut mail = Replica::<Mail>::new();
        let content = tilo_content::ContentStore::memory();
        let attachment =
            crate::attachment_store::stage_bytes(&content, b"draft blob", "agenda.txt")
                .expect("portable attachment");
        let draft = Draft {
            to: "x@y.example".into(),
            subject: "Hi".into(),
            body: "Body".into(),
            attachments: vec![attachment.clone()],
            ..Default::default()
        };
        save(&mut mail, &draft);
        let loaded = mail.state().unwrap().draft.draft();
        assert_eq!(loaded.to, "x@y.example");
        assert_eq!(loaded.subject, "Hi");
        assert_eq!(loaded.attachments, vec![attachment]);
        mail.apply_confirmed(Mail::DISCARD_DRAFT.with(mailbox::DiscardDraft {}))
            .unwrap();
        assert!(mail.state().unwrap().draft.is_empty(), "discard clears it");
    }

    // Phase `labels_add_remove_and_stay_a_set`.
    {
        let mut mail = Replica::<Mail>::new();
        register(&mut mail, "m1", Folder::Inbox);
        let labels = at("m1").labels;
        edit(&mut mail, &labels.insert(&"work".to_owned())).unwrap();
        edit(&mut mail, &labels.insert(&"urgent".to_owned())).unwrap();
        edit(&mut mail, &labels.insert(&"work".to_owned())).unwrap();
        let read = |mail: &Replica<Mail>| -> Vec<String> {
            state(mail, "m1").unwrap().labels.iter().cloned().collect()
        };
        assert_eq!(read(&mail), ["urgent", "work"]);
        edit(&mut mail, &labels.remove(&"work".to_owned())).unwrap();
        assert_eq!(read(&mail), ["urgent"]);
        assert!(
            edit(&mut mail, &labels.insert(&"  ".to_owned())).is_err(),
            "a label has a name"
        );
        assert!(
            edit(&mut mail, &labels.insert(&"🦀".repeat(200))).is_err(),
            "a label is short"
        );
    }

    // Phase `local_folder_registry_crud`.
    {
        let mut mail = Replica::<Mail>::new();
        let folders = Mail::paths().folders;
        let folder = |name: &str| crate::resource::LocalFolder { name: name.into() };
        edit(
            &mut mail,
            &folders.insert_as(&Id::from_raw("f-a"), 0, &folder("Receipts")),
        )
        .unwrap();
        edit(
            &mut mail,
            &folders.insert_as(&Id::from_raw("f-b"), 1, &folder("Travel")),
        )
        .unwrap();
        edit(
            &mut mail,
            &folders.item(&Id::from_raw("f-a")).name.replace("Invoices"),
        )
        .unwrap();
        assert!(
            edit(
                &mut mail,
                &folders.item(&Id::from_raw("ghost")).name.replace("X")
            )
            .is_err(),
            "renaming an unknown folder changes nothing"
        );
        assert!(
            edit(
                &mut mail,
                &folders.item(&Id::from_raw("f-a")).name.replace(" ")
            )
            .is_err(),
            "a folder has a name"
        );
        mail.apply_confirmed(Invoke::edit(&folders.remove(&Id::from_raw("f-b"))).unwrap())
            .unwrap();
        assert_eq!(
            mail.state().unwrap().custom_folders(),
            [("f-a".to_owned(), "Invoices".to_owned())]
        );
    }

    // Phase `saving_an_unchanged_draft_writes_nothing`.
    {
        let mut mail = Replica::<Mail>::new();
        let draft = Draft {
            to: "x@y.example".into(),
            subject: "Hi".into(),
            body: "Body".repeat(50),
            ..Default::default()
        };
        save(&mut mail, &draft);
        let a = mail.save().len();
        for _ in 0..50 {
            save(&mut mail, &draft);
        }
        assert_eq!(
            a,
            mail.save().len(),
            "re-saving an unchanged draft grew the mailbox"
        );
    }

    // Phase `draft_text_is_unicode_safe_and_bounded`.
    {
        let mut mail = Replica::<Mail>::new();
        let draft = Draft {
            subject: "é".repeat(MAX_DRAFT_SUBJECT_CHARS + 10),
            body: format!("{}\0", "🦀".repeat(MAX_DRAFT_BODY_CHARS + 10)),
            ..Draft::default()
        };
        save(&mut mail, &draft);
        let stored = mail.state().unwrap().draft.draft();
        assert_eq!(stored.subject.chars().count(), MAX_DRAFT_SUBJECT_CHARS);
        assert_eq!(stored.body.chars().count(), MAX_DRAFT_BODY_CHARS);
        assert!(!stored.body.contains('\0'));
        assert!(
            edit(
                &mut mail,
                &Mail::paths()
                    .draft
                    .subject
                    .replace("é".repeat(2 * MAX_DRAFT_SUBJECT_CHARS))
            )
            .is_err(),
            "a subject written at its path is held to the same bound"
        );
    }
}

/// Save `draft` as the composer does: the fields that changed.
fn save(mail: &mut Replica<Mail>, draft: &Draft) {
    apply(
        mail,
        Mail::SAVE_DRAFT.with(mailbox::SaveDraft {
            message: DraftData::from_draft(draft),
        }),
    )
    .unwrap();
}

/// Where the mailbox and the server differ, the server has to be told; once
/// it confirms, nothing is left. A change taken back before then leaves
/// nothing to tell, and a message that exists only here never goes to a
/// server.
#[test]
fn server_updates_follow_the_mailbox_until_the_server_confirms() {
    let mut mail = Replica::<Mail>::new();
    ingest(&mut mail, vec![fetched("m1", false, Folder::Inbox)]);
    let updates = |mail: &Replica<Mail>| mail.state().unwrap().server_updates();
    assert!(updates(&mail).is_empty(), "nothing changed");

    let mark = |mail: &mut Replica<Mail>, read| {
        apply(
            mail,
            Mail::MARK_READ.with(mailbox::MarkRead {
                messages: ids(&["m1"]),
                read,
            }),
        )
        .unwrap();
    };
    let confirm = |mail: &mut Replica<Mail>, update: &ServerUpdate| {
        apply(
            mail,
            Mail::SERVER_APPLIED.with(mailbox::ServerApplied {
                update: update.clone(),
            }),
        )
        .unwrap();
    };
    mark(&mut mail, true);
    let flags = ServerUpdate::Flags {
        id: "m1".into(),
        account: "you@example.test".into(),
        locator: "m1@example.test".into(),
        folder: Folder::Inbox,
        read: true,
        starred: false,
    };
    assert_eq!(updates(&mail), std::slice::from_ref(&flags));
    confirm(&mut mail, &flags);
    assert!(updates(&mail).is_empty());

    mark(&mut mail, false);
    mark(&mut mail, true);
    assert!(
        updates(&mail).is_empty(),
        "a change taken back leaves nothing to tell"
    );

    edit(&mut mail, &at("m1").folder.set(Folder::Trash)).unwrap();
    let moved = ServerUpdate::Move {
        id: "m1".into(),
        account: "you@example.test".into(),
        locator: "m1@example.test".into(),
        from: Folder::Inbox,
        to: Folder::Trash,
    };
    assert_eq!(updates(&mail), std::slice::from_ref(&moved));
    confirm(&mut mail, &moved);
    assert!(updates(&mail).is_empty());

    mail.apply_confirmed(Mail::PURGE.with(mailbox::Purge {
        messages: ids(&["m1"]),
    }))
    .unwrap();
    let removed = ServerUpdate::Remove {
        id: "m1".into(),
        account: "you@example.test".into(),
        locator: "m1@example.test".into(),
        folder: Folder::Trash,
    };
    assert_eq!(updates(&mail), std::slice::from_ref(&removed));
    confirm(&mut mail, &removed);
    assert!(updates(&mail).is_empty());

    let mut local =
        IngestBaseline::from_message(&sample_mailbox().messages.values().next().cloned().unwrap());
    local.id = "draft".into();
    ingest(&mut mail, vec![local]);
    edit(&mut mail, &at("draft").starred.set(true)).unwrap();
    assert!(updates(&mail).is_empty(), "a local message stays local");
}

/// A flag the server changed since it last reported it (another mail client
/// read the message) is adopted; one it reports unchanged never overwrites a
/// change still on its way to it.
#[test]
fn a_fetch_adopts_server_changes_and_keeps_changes_on_their_way() {
    let mut mail = Replica::<Mail>::new();
    ingest(&mut mail, vec![fetched("m1", false, Folder::Inbox)]);
    ingest(&mut mail, vec![fetched("m1", true, Folder::Inbox)]);
    assert!(state(&mail, "m1").unwrap().read);
    assert!(mail.state().unwrap().server_updates().is_empty());

    edit(&mut mail, &at("m1").starred.set(true)).unwrap();
    ingest(&mut mail, vec![fetched("m1", true, Folder::Inbox)]);
    assert!(state(&mail, "m1").unwrap().starred);
    assert_eq!(
        mail.state().unwrap().server_updates().len(),
        1,
        "the star still has to go"
    );

    ingest(&mut mail, vec![fetched("m1", true, Folder::Archive)]);
    assert_eq!(
        state(&mail, "m1").unwrap().folder,
        Folder::Archive,
        "a move made elsewhere is adopted"
    );
}

/// Batch verbs act on every message they name or refuse; purge deletes all
/// of them or none, a stale or unknown id changes nothing, two readers'
/// flags on one message merge.
#[test]
fn batch_verbs_fail_closed_and_converge() {
    // Phase `purge_refuses_mixed_or_stale_ids_without_deleting_a_valid_subset`.
    {
        let mut mail = Replica::<Mail>::new();
        register(&mut mail, "inbox", Folder::Inbox);
        register(&mut mail, "trash", Folder::Trash);
        let before = mail.heads();
        assert!(
            mail.apply_confirmed(Mail::PURGE.with(mailbox::Purge {
                messages: ids(&["inbox", "missing", "trash"]),
            }))
            .is_err(),
            "mixed/stale purge refuses rather than deleting a valid subset"
        );
        assert!(state(&mail, "inbox").is_some() && state(&mail, "trash").is_some());
        assert_eq!(mail.heads(), before);
    }

    // Phase `purge_merges_to_peers_and_refuses_late_flag_resurrection`.
    {
        let mut a = Replica::<Mail>::new();
        register(&mut a, "m", Folder::Trash);
        let mut b = a.fork();
        a.apply_confirmed(Mail::PURGE.with(mailbox::Purge {
            messages: ids(&["m"]),
        }))
        .unwrap();
        a.sync_with(&mut b).unwrap();
        assert!(state(&b, "m").is_none());
        assert!(
            apply(
                &mut b,
                Mail::MARK_READ.with(mailbox::MarkRead {
                    messages: ids(&["m"]),
                    read: true,
                })
            )
            .is_err(),
            "a deleted message is not marked read"
        );
    }

    // Phase `unknown_messages_are_refused_without_orphan_flags`.
    {
        let mut mail = Replica::<Mail>::new();
        assert!(
            apply(
                &mut mail,
                Mail::MARK_READ.with(mailbox::MarkRead {
                    messages: ids(&["not-a-loaded-message"]),
                    read: true,
                })
            )
            .is_err()
        );
        assert!(
            edit(&mut mail, &at("not-a-loaded-message").starred.set(true)).is_err(),
            "a stale palette row cannot mint flags"
        );
        assert!(mail.state().unwrap().messages.is_empty());
    }

    // Phase `duplicate_and_excess_ids_are_one_bounded_target_set`.
    {
        let mut mail = Replica::<Mail>::new();
        register(&mut mail, "m", Folder::Inbox);
        let many = std::iter::repeat_n("m".to_string(), MAX_MESSAGES_PER_ACTION + 20).collect();
        apply(
            &mut mail,
            Mail::ARCHIVE.with(mailbox::Archive { messages: many }),
        )
        .unwrap();
        assert_eq!(state(&mail, "m").unwrap().folder, Folder::Archive);
    }

    // Phase `concurrent_same_message_flag_edits_merge_without_losing_either_field`.
    {
        let mut read_replica = Replica::<Mail>::new();
        register(&mut read_replica, "m", Folder::Inbox);
        let mut star_replica = read_replica.fork();
        apply(
            &mut read_replica,
            Mail::MARK_READ.with(mailbox::MarkRead {
                messages: ids(&["m"]),
                read: true,
            }),
        )
        .unwrap();
        edit(&mut star_replica, &at("m").starred.set(true)).unwrap();
        read_replica.sync_with(&mut star_replica).unwrap();
        let merged = state(&read_replica, "m").unwrap();
        assert!(merged.read && merged.starred);
        assert_eq!(read_replica.state().unwrap(), star_replica.state().unwrap());
    }
}

/// The draft body is rich text, started by a verb and edited at its path.
#[test]
fn the_draft_body_is_rich_text_started_and_edited_at_its_path() {
    use tilo_document::action::RegionKind;

    let mut mail = Replica::<Mail>::new();
    let body = crate::resource::draft_body().some();
    let text = |mail: &Replica<Mail>| {
        let doc = mail.doc();
        let root = crate::resource::draft_body_object(doc).expect("a body");
        tilo_rich_text::blocks_under(doc, &root)
            .unwrap()
            .into_iter()
            .map(|block| (block.kind, block.text, block.id))
            .collect::<Vec<_>>()
    };
    assert!(crate::resource::draft_body_object(mail.doc()).is_none());
    assert!(
        edit(
            &mut mail,
            &body
                .perform(&RichTextAction::AppendParagraph { text: "x".into() })
                .unwrap()
        )
        .is_err(),
        "no body to edit yet"
    );

    // A reply quote: a paragraph per line, `> ` lines quoted.
    apply(
        &mut mail,
        Mail::START_DRAFT_BODY.with(mailbox::StartDraftBody {
            text: "Thanks!\n> original *as is*".into(),
            html: None,
        }),
    )
    .unwrap();
    let blocks = text(&mail);
    assert_eq!(
        blocks
            .iter()
            .map(|(kind, text, _)| (kind.clone(), text.as_str()))
            .collect::<Vec<_>>(),
        [
            (ui::widget::BlockKind::Paragraph, "Thanks!"),
            (ui::widget::BlockKind::Quote, "original *as is*"),
        ]
    );
    edit(
        &mut mail,
        &body
            .perform(&RichTextAction::SpliceText {
                region: RegionKind::Body,
                block: blocks[0].2.clone(),
                at: 6,
                del: 1,
                ins: ", Ada.".into(),
            })
            .unwrap(),
    )
    .unwrap();
    assert_eq!(text(&mail)[0].1, "Thanks, Ada.");

    // A reopened draft starts from its HTML part.
    apply(
        &mut mail,
        Mail::START_DRAFT_BODY.with(mailbox::StartDraftBody {
            text: "Plan".into(),
            html: Some("<h2>Plan</h2><p><b>ship</b> it</p>".into()),
        }),
    )
    .unwrap();
    assert_eq!(
        text(&mail)
            .into_iter()
            .map(|(kind, text, _)| (kind, text))
            .collect::<Vec<_>>(),
        [
            (ui::widget::BlockKind::Heading(2), "Plan".to_owned()),
            (ui::widget::BlockKind::Paragraph, "ship it".to_owned()),
        ]
    );
    mail.apply_confirmed(Mail::DISCARD_DRAFT.with(mailbox::DiscardDraft {}))
        .unwrap();
    assert!(
        crate::resource::draft_body_object(mail.doc()).is_none(),
        "discard clears the body"
    );
}

// --- the mailbox as the window publishes it --------------------------------

/// The sample mailbox window as the framework hosts it.
fn sample_window() -> TestApp<MailShell> {
    ui::i18n::I18nManager::init("en-US");
    TestApp::<MailShell>::capture(Some("wide")).expect("the sample mailbox captures")
}

/// Everything the window publishes, as the host's palette and agents see it.
fn published(app: &mut TestApp<MailShell>) -> Vec<Resource> {
    app.harness().resources()
}

/// The published mailbox root.
fn mailbox_root(resources: &[Resource]) -> &Resource {
    let mut roots = resources.iter().filter(|r| r.kind == "mail.mailbox");
    let root = roots.next().expect("a published mailbox");
    assert!(roots.next().is_none(), "one mailbox root");
    root
}

/// The published resource `id`, wherever it sits in the tree.
fn find<'a>(resources: &'a [Resource], id: &str) -> Option<&'a Resource> {
    resources.iter().find_map(|resource| {
        if resource.id.as_str() == id {
            Some(resource)
        } else {
            find(&resource.children, id)
        }
    })
}

/// Resolve `id` as the host resolves an address the tree does not list.
fn resolve(app: &mut TestApp<MailShell>, id: &str) -> Option<Resource> {
    let harness = app.harness();
    ui::resources::resource(harness.root_widget(), &harness.state().doc, id)
}

/// The ids of the mailbox's messages in the Inbox.
fn inbox_ids(mail: &Mail) -> Vec<String> {
    mail.messages
        .iter()
        .filter(|(_, message)| !message.purged && message.folder == Folder::Inbox)
        .map(|(id, _)| id.as_str().to_owned())
        .collect()
}

fn message_place(id: &str) -> String {
    format!(
        "{}/{}",
        folder_resource(Folder::Inbox.as_key()),
        crate::resource::message_place_id(id)
    )
}

/// The mailbox tree: open vs neutral preview, a bounded page of messages
/// under each folder, stable thread/account addresses, and purge on what is
/// in the Trash.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn mailbox_publishes_bounded_stable_places() {
    // Phase `a_loaded_message_distinguishes_open_from_read_state_neutral_preview`.
    {
        let mut app = sample_window();
        let unread = app
            .with_surface::<MailApp, _>(|mail| {
                mail.shared_mailbox()
                    .messages
                    .values()
                    .find(|message| message.folder == Folder::Inbox && !message.read)
                    .map(|message| message.id.to_string())
            })
            .flatten()
            .expect("sample has an unread inbox message");
        let resource =
            resolve(&mut app, &format!("mail.message:{unread}")).expect("the loaded message");
        let open = resource
            .actions
            .iter()
            .find(|action| action.id == "open")
            .expect("normal open action");
        let preview = resource
            .actions
            .iter()
            .find(|action| action.id == "inspect")
            .expect("explicit preview action");
        assert_eq!(open.label, "Open message");
        assert_eq!(preview.label, "Preview without marking read");
        for id in ["reply", "reply-all", "forward"] {
            assert!(
                resource.actions.iter().any(|action| action.id == id),
                "a message offers {id}"
            );
        }
        let payload = |spec: &ActionSpec| {
            spec.build_json(&serde_json::Map::new())
                .expect("a bound view takes no arguments")
                .payload
        };
        assert!(matches!(
            MailEffect::from_payload(&payload(open)).unwrap(),
            MailEffect::Open {
                target: MailTarget::Message(_)
            }
        ));
        assert!(matches!(
            MailEffect::from_payload(&payload(preview)).unwrap(),
            MailEffect::Inspect { .. }
        ));
    }

    // Phase `ctrl_k_publishes_the_mailbox_tree`.
    {
        let mut live = TestApp::<MailShell>::new().unwrap();
        let live_resources = published(&mut live);
        let live_mailbox = mailbox_root(&live_resources);
        let effects: Vec<&str> = live_mailbox.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(effects.contains(&"compose"), "{effects:?}");
        assert!(effects.contains(&"refresh"), "{effects:?}");
        ui::testing::action_contract::assert_ctrl_k_publishes(
            &live_resources,
            &["Compose message", "Archive messages"],
        );
        let refresh = live_mailbox
            .actions
            .iter()
            .find(|a| a.id == "refresh")
            .unwrap();
        assert_eq!(
            refresh.contract.execution,
            tilo_actions::ExecutionMode::Background { cancellable: false }
        );
        assert_eq!(refresh.contract.result, tilo_actions::ResultSpec::Job);

        let mut app = sample_window();
        let resources = published(&mut app);
        let mut registry = ActionRegistry::new();
        Mail::register(&mut registry);
        let violations = tilo_actions::validate_surface_contracts(&resources, &registry);
        assert!(violations.is_empty(), "{violations:?}");
        let root = mailbox_root(&resources);
        let actions: Vec<&str> = root.actions.iter().map(|a| a.id.as_str()).collect();
        for id in [
            "archive",
            "trash",
            "move-messages",
            "mark-read",
            "purge",
            "send-message",
            "cancel-send",
            "field-edit",
        ] {
            assert!(
                actions.contains(&id),
                "the mailbox offers {id}: {actions:?}"
            );
        }

        let inbox = find(&resources, &folder_resource(Folder::Inbox.as_key()))
            .expect("the inbox folder place");
        assert!(
            !inbox.children.is_empty() && inbox.children.len() <= OPEN_MESSAGE_LIMIT,
            "the inbox publishes a bounded page of its messages"
        );
        assert!(
            inbox
                .children
                .iter()
                .all(|item| item.kind == "mail.mailbox.message"),
            "an inbox child is a message"
        );

        // A loaded thread stays addressable though the tree does not list it.
        let thread = app
            .with_surface::<MailApp, _>(|mail| {
                mail.shared_mailbox().threads_in(Folder::Inbox)[0].clone()
            })
            .expect("the window is Mail");
        let resolved = resolve(&mut app, &format!("mail.thread:inbox:{}", thread.id))
            .expect("a loaded thread resolves by its stable address");
        assert_eq!(resolved.kind, "mail.thread");
        assert_eq!(resolved.children.len(), thread.message_ids.len());

        let id = inbox_ids(&app.state())[0].clone();
        let message = find(&resources, &message_place(&id)).expect("the message's place");
        let place_actions: Vec<&str> = message.actions.iter().map(|a| a.id.as_str()).collect();
        for want in ["open", "inspect", "reply", "mark-read", "archive", "trash"] {
            assert!(
                place_actions.contains(&want),
                "a message offers {want}: {place_actions:?}"
            );
        }
        assert_eq!(
            message
                .actions
                .iter()
                .find(|a| a.id == "trash")
                .unwrap()
                .contract
                .risk,
            tilo_actions::RiskLevel::Low,
            "moving to Trash can be taken back"
        );
    }

    // Phase `a_large_mailbox_does_not_expand_the_published_tree`.
    {
        let mut app = TestApp::<MailShell>::new().unwrap();
        let count = OPEN_MESSAGE_LIMIT * 5;
        app.invoke(
            Mail::INGEST.with(mailbox::Ingest {
                messages: (0..count)
                    .map(|id| IngestBaseline {
                        folder: Some("inbox".into()),
                        ..IngestBaseline::register_only(id.to_string())
                    })
                    .collect(),
            }),
        )
        .unwrap();
        let resources = published(&mut app);
        let inbox = find(&resources, &folder_resource("inbox")).expect("inbox place");
        assert_eq!(
            inbox.children.len(),
            OPEN_MESSAGE_LIMIT,
            "the published page is bounded"
        );
        let last = (count - 1).to_string();
        assert!(find(&resources, &message_place(&last)).is_none());
        app.invoke(Mail::ARCHIVE.with(mailbox::Archive {
            messages: vec![last.clone()],
        }))
        .unwrap();
        assert_eq!(
            app.state().message(&last).unwrap().folder,
            Folder::Archive,
            "a message past the page is still acted on by its id"
        );
    }

    // Phase `threads_keep_distinct_stable_resource_addresses`.
    {
        let mut app = sample_window();
        let threads = app
            .with_surface::<MailApp, _>(|mail| mail.shared_mailbox().threads_in(Folder::Inbox))
            .expect("the window is Mail");
        assert!(threads.len() > 1, "the sample inbox has several threads");
        let mut seen = std::collections::HashSet::new();
        for thread in threads {
            let id = format!("mail.thread:inbox:{}", thread.id);
            assert!(seen.insert(id.clone()), "{id} names one thread");
            let resolved = resolve(&mut app, &id).expect("the thread resolves");
            let messages: Vec<String> = resolved
                .children
                .iter()
                .map(|message| message.id.as_str().to_owned())
                .collect();
            let expected: Vec<String> = thread
                .message_ids
                .iter()
                .map(|message| format!("mail.message:{message}"))
                .collect();
            assert_eq!(messages, expected, "{id}");
        }
    }

    // Phase `account_resource_uses_a_durable_id_not_its_display_index`.
    {
        let mut app = sample_window();
        let resources = published(&mut app);
        let accounts = find(&resources, "mail.accounts").expect("the accounts row");
        assert_eq!(accounts.children.len(), 2, "both sample accounts");
        let other = accounts
            .children
            .iter()
            .find(|account| !account.label.ends_with("· Active"))
            .expect("an inactive account");
        let raw = other
            .id
            .as_str()
            .strip_prefix("mail.account:")
            .expect("an account address")
            .to_owned();
        assert!(
            raw.parse::<usize>().is_err(),
            "an account is addressed by its id, not its position: {raw}"
        );
        let encoded = other
            .actions
            .iter()
            .find(|action| action.id == "open")
            .expect("the account offers to open it")
            .build_json(&serde_json::Map::new())
            .expect("the stable account address opens it");
        assert_eq!(
            MailEffect::from_payload(&encoded.payload).unwrap(),
            MailEffect::Open {
                target: MailTarget::Account(raw)
            }
        );
    }

    // Phase `a_trashed_message_offers_its_permanent_deletion_confirmed`.
    {
        let mut mail = Replica::<Mail>::new();
        register(&mut mail, "m", Folder::Trash);
        let tree = mail.published().unwrap();
        let place = find(
            std::slice::from_ref(&tree),
            &format!(
                "{}/{}",
                folder_resource("trash"),
                crate::resource::message_place_id("m")
            ),
        )
        .expect("the trashed message's place");
        let purge = place
            .actions
            .iter()
            .find(|action| action.id == "purge")
            .expect("permanent deletion");
        assert!(matches!(
            purge.contract.confirmation,
            tilo_actions::ConfirmationPolicy::Confirm
        ));
        assert!(place.actions.iter().all(|action| action.id != "trash"));
    }
}

/// The window a contract seed was staged in, and the message the seed
/// moved to Trash: the resources are what that very window publishes.
struct Seeded {
    app: TestApp<MailShell>,
    trashed: String,
}

thread_local! {
    static SEEDED: std::cell::RefCell<Option<Seeded>> = const { std::cell::RefCell::new(None) };
}

fn with_seeded<R>(f: impl FnOnce(&mut Seeded) -> R) -> R {
    SEEDED.with_borrow_mut(|seeded| f(seeded.as_mut().expect("the contract seed ran first")))
}

/// The sample mailbox after a reader trashed one message, labelled another,
/// created a local folder and saved a draft — all through the mailbox.
fn mailbox_seed() -> AutoCommit {
    let mut app = sample_window();
    let ids = inbox_ids(&app.state());
    let trashed = ids.last().cloned().expect("sample inbox has messages");
    let before = app.state().draft;
    let draft = before.saved(&Draft {
        to: "ada@example.test".into(),
        subject: "Plans".into(),
        ..Default::default()
    });
    let mut edits = vec![
        at(&trashed).folder.set(Folder::Trash),
        at(&ids[0]).labels.insert(&"work".to_owned()),
        Mail::paths().folders.insert_as(
            &Id::from_raw("receipts"),
            0,
            &crate::resource::LocalFolder {
                name: "Receipts".into(),
            },
        ),
    ];
    edits.extend(Mail::paths().draft.path().edits(&before, &draft));
    for change in edits {
        app.invoke(Invoke::edit(&change).unwrap()).unwrap();
    }
    app.settle();
    let doc = app.read_document(Clone::clone);
    SEEDED.set(Some(Seeded { app, trashed }));
    doc
}

fn registry() -> ActionRegistry {
    let mut registry = ActionRegistry::new();
    Mail::register(&mut registry);
    registry
}

fn encoded(invoke: &Invoke<Mail>) -> tilo_actions::EncodedAction {
    let action = invoke.build(1_700_000_000_000).unwrap();
    ui::tilo_resource::encode::<Mail>(&action).unwrap()
}

fn read(doc: &AutoCommit) -> Mail {
    crate::resource::read_mail(doc).expect("a mailbox")
}

ui::action_contract_tests! {
    seed: mailbox_seed(),
    registry: registry(),
    resources: |_| with_seeded(|seeded| published(&mut seeded.app)),
    palette: ["Archive messages", "Discard draft"],
    replay: [
        star => |doc| {
            let mail = read(doc);
            let id = inbox_ids(&mail)
                .into_iter()
                .find(|id| mail.message(id).is_some_and(|message| !message.starred))
                .expect("an unstarred inbox message");
            encoded(&Invoke::edit(&at(&id).starred.set(true)).unwrap())
        },
        batched_archive => |doc| encoded(&Mail::ARCHIVE.with(mailbox::Archive {
            messages: inbox_ids(&read(doc)).into_iter().take(3).collect(),
        })),
        add_label => |doc| encoded(&
            Invoke::edit(&at(&inbox_ids(&read(doc))[1]).labels.insert(&"urgent".to_owned()))
                .unwrap(),
        ),
        purge => |_| encoded(&Mail::PURGE.with(mailbox::Purge {
            messages: vec![with_seeded(|seeded| seeded.trashed.clone())],
        })),
    ],
    undo: [
        move_to_trash => |doc| encoded(&Mail::TRASH.with(mailbox::Trash {
            messages: vec![inbox_ids(&read(doc))[0].clone()],
        })),
    ],
}

/// The on-screen affordance and the palette make the identical edit: the
/// message's archive control and the place's archive verb.
#[test]
fn the_archive_affordance_and_the_palette_edit_alike() {
    let mut st = MailState::new_sample(crate::avatars::Avatars::new());
    let mut doc = Replica::<Mail>::new().doc().clone();
    let id = st.mailbox.messages_in(Folder::Inbox)[0].id.to_string();
    st.apply_message_action(
        &mut ui::widget::Edits::lend(&mut doc),
        id.parse().unwrap(),
        CtxAction::Archive,
    );
    let mut palette = Replica::<Mail>::new();
    register(&mut palette, &id, Folder::Inbox);
    let state = palette.state().unwrap();
    let place = Mail::message_place(&id, state.message(&id).unwrap()).publish();
    let archive = place
        .actions
        .iter()
        .find(|action| action.id == "archive")
        .unwrap();
    let action: <Mail as ui::tilo_resource::Resource>::Action =
        serde_json::from_slice(&archive.build_json(&serde_json::Map::new()).unwrap().payload)
            .unwrap();
    palette.apply_action(&action).unwrap();
    let folder = |mail: &Mail| mail.message(&id).map(|message| message.folder);
    assert_eq!(folder(&read(&doc)), Some(Folder::Archive));
    assert_eq!(folder(&read(&doc)), folder(&palette.state().unwrap()));
}

/// An agent sends in one call and the message waits in the outbox; undo
/// takes it back unsent before a device holding the account delivers it,
/// and history reads it back as the app's sentence.
#[test]
fn a_message_is_sent_through_the_outbox_and_undo_takes_it_back_unsent() {
    use ui::tilo_actions::{ActionInvoker, ActorKind};

    let mut app = TestApp::<MailShell>::new().unwrap();
    let agent = ActionInvoker::actor("agent:claude")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    let message = DraftData::from_draft(&Draft {
        to: "you@example.test".into(),
        subject: "Lunch".into(),
        body: "Noon?".into(),
        ..Draft::default()
    });
    assert!(
        app.call_by(
            agent,
            "mail.mailbox",
            "send-message",
            serde_json::json!({ "account": "nobody-here@example.test", "message": message }),
        )
        .is_err(),
        "sending asks for confirmation"
    );
    app.invoke_confirmed(Mail::SEND_MESSAGE.with(mailbox::SendMessage {
        account: "nobody-here@example.test".into(),
        message: message.clone(),
        in_reply_to: None,
    }))
    .unwrap();
    app.settle();
    let outbox = app.state().outgoing();
    assert_eq!(outbox.len(), 1, "{outbox:?}");
    assert_eq!(outbox[0].0.draft.subject, "Lunch");
    assert!(
        outbox[0].1.is_none(),
        "a device without the account leaves it waiting, not refused"
    );
    assert!(app.undo(), "sending is an undo step until it is delivered");
    assert!(
        app.state().outgoing().is_empty(),
        "undo took it back unsent"
    );
    assert!(
        app.invoke_confirmed(Mail::SEND_MESSAGE.with(mailbox::SendMessage {
            account: "nobody-here@example.test".into(),
            message: DraftData::default(),
            in_reply_to: None,
        }))
        .is_err(),
        "a message without a recipient is refused"
    );

    app.invoke_confirmed(Mail::SEND_MESSAGE.with(mailbox::SendMessage {
        account: "nobody-here@example.test".into(),
        message,
        in_reply_to: None,
    }))
    .unwrap();
    let id = app.state().outgoing()[0].0.id.clone();
    app.invoke(Mail::CANCEL_SEND.with(mailbox::CancelSend { id }))
        .unwrap();
    assert!(app.state().outgoing().is_empty(), "cancelled unsent");
}

/// What an agent does for a person reads back in the reader's language as
/// the app's sentences.
#[test]
fn history_entries_render_as_the_apps_sentences() {
    use ui::tilo_actions::{ActionInvoker, ActorKind};

    let mut app = TestApp::<MailShell>::new().unwrap();
    app.invoke(Mail::INGEST.with(mailbox::Ingest {
        messages: vec![IngestBaseline {
            folder: Some("inbox".into()),
            ..IngestBaseline::register_only("m1")
        }],
    }))
    .unwrap();
    let agent = ActionInvoker::actor("agent:claude")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    for change in [
        at("m1").starred.set(true),
        at("m1").labels.insert(&"work".to_owned()),
    ] {
        app.call_by(
            agent.clone(),
            "mail.mailbox",
            "field-edit",
            serde_json::json!({ "edit": change }),
        )
        .unwrap();
    }
    app.call_by(
        agent.clone(),
        "mail.mailbox",
        "archive",
        serde_json::json!({ "messages": ["m1"] }),
    )
    .unwrap();
    app.call_by(
        agent,
        "mail.mailbox",
        "mark-read",
        serde_json::json!({ "messages": ["m1"], "read": true }),
    )
    .unwrap();

    let sentences = app.sentences(app.history().iter().filter(|entry| {
        entry
            .invoker
            .as_ref()
            .is_some_and(|invoker| invoker.actor == "agent:claude")
    }));
    assert_eq!(
        sentences,
        [
            "Starred a message",
            "Changed the labels of a message",
            "Archived messages",
            "Marked messages read",
        ]
    );
}
