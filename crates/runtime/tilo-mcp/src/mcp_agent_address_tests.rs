//! The names a path may take: the address an action answers, the same path
//! after a restart, and the titles a listing tells apart by their places.

use super::*;

tilo_test_env::scenario! {
    fn a_path_a_name_or_an_address_leads_to_one_resource_or_says_what_was_meant;

    phase a_resource_is_named_by_the_address_an_action_answers;
    // A resource is also named by the address an action answers it by
    // (`notes.notebook/page/<id>/body`): what a call said it changed is a path
    // the next call takes as it is.
    {
        let host = Notes::new(true);
        run(async {
            let address = format!("{TODO_BODY}#append-paragraph");
            let (text, failed) = tool(
                &agent(&host),
                "tilo_do",
                json!({"path": address, "args": {"text": "x"}}),
            )
            .await;
            assert!(!failed, "{text}");
            let invoked = host.invoked();
            let (resource, action, _) = invoked.last().unwrap();
            assert_eq!(*resource, host.id(TODO_BODY));
            assert_eq!(action, "append-paragraph");

            // Without an action, the same address lists the same resource.
            let (text, failed) = tool(&agent(&host), "tilo_ls", json!({"path": TODO_BODY})).await;
            assert!(!failed, "{text}");
            assert!(text.contains("#append-paragraph(text)"), "{text}");
        });
    }

    phase a_path_means_the_same_resource_after_the_desktop_restarts;
    {
        let before = Notes::new(true);
        let after = before.restarted();
        assert_ne!(
            before.id(TODO_BODY),
            after.id(TODO_BODY),
            "the broker's ids moved"
        );
        run(async {
            for host in [&before, &after] {
                let (text, failed) = tool(
                    &agent(host),
                    "tilo_do",
                    json!({"path": "notes/Todo/body#append-paragraph", "args": {"text": "x"}}),
                )
                .await;
                assert!(!failed, "{text}");
                let invoked = host.invoked();
                let (resource, action, _) = invoked.last().unwrap();
                assert_eq!(*resource, host.id(TODO_BODY));
                assert_eq!(action, "append-paragraph");
            }
        });
    }

    phase the_app_alone_names_an_action_it_publishes_on_a_resource_it_lists;
    // The live Settings publishes `add-email-account` on the root resource it
    // lists, not on the app's own root: `settings#add-email-account` names it
    // as surely as `settings/settings.root#add-email-account` does.
    {
        let host = Notes::new(true).live();
        run(async {
            let (text, failed) = tool(&agent(&host), "tilo_do", json!({"path": "notes#archive"})).await;
            assert!(!failed, "{text}");
            assert_eq!(text, "ok", "{text}");
            let (resource, action, _) = host.invoked().last().cloned().unwrap();
            assert_eq!(resource, host.id(&Notes::live_key(TODO)), "{resource}");
            assert_eq!(action, "archive");
            let (text, failed) = tool(&agent(&host), "tilo_ls", json!({"path": "notes#archive"})).await;
            assert!(!failed, "{text}");
            assert!(
                text.starts_with("notes/Notebook/Todo#archive  Archive note"),
                "{text}"
            );
        });
    }

    phase an_app_action_on_two_resources_is_refused_naming_both;
    // Two resources carrying the same action is not one action: the app alone
    // names neither, and says which it would have to be told.
    {
        let host = Notes::new(true).live().archiving_both_notes();
        run(async {
            let (text, failed) = tool(&agent(&host), "tilo_do", json!({"path": "notes#archive"})).await;
            assert!(failed, "{text}");
            assert!(
                text.contains("names 2 actions")
                    && text.contains("notes/Notebook/Todo#archive")
                    && text.contains("notes/Notebook/Groceries#archive"),
                "{text}"
            );
            assert!(text.contains("#archive"), "{text}");
            assert!(host.invoked().is_empty(), "nothing ran: {text}");
        });
    }
}

/// A listing says each item by its title; where two titles read the same, each
/// is named by its place among them (`Todo`, `Todo 2`), a name a path takes,
/// and never by its id.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_pages_of_one_title_are_told_apart_by_their_places() {
    let notes = Notes::new(true);
    let pages = [notes.id(TODO), notes.id(SHOP)];
    let rows: Vec<CatalogueRow> = notes
        .rows("1")
        .into_iter()
        .map(|mut row| {
            if pages.contains(&row.resource.id) {
                row.resource.label = "Todo".into();
                if let Some(action) = row.action.as_mut() {
                    action.resource_label = "Todo".into();
                }
            }
            row
        })
        .collect();
    *notes.published_kinds.lock().unwrap() = Some(rows);
    let (text, failed) = tool(&agent(&notes), "tilo_ls", json!({"path": "notes"})).await;
    assert!(!failed, "{text}");
    for line in ["Todo/  notes.page", "Todo 2/  notes.page"] {
        assert!(text.lines().any(|found| found == line), "{line} in\n{text}");
    }
    for page in &pages {
        assert!(
            !text.contains(page.as_str()),
            "a page is named by its id in\n{text}"
        );
    }
    // The name the listing printed is a path.
    let (page, failed) = tool(&agent(&notes), "tilo_ls", json!({"path": "notes/Todo 2"})).await;
    assert!(!failed, "{page}");
    assert!(page.contains("notes/Todo 2"), "{page}");
}
