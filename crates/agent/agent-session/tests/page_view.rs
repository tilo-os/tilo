//! A document's view shown as a web page by its app worker: the page a person's
//! browser renders, and what they do on it acted on the view as a click or
//! typing in a window would be, its changes handed back to be saved.

use crate::common::{WORKSPACE, catalogue};
use tilo_actions::MemberId;
use tilo_agent_session::{ConfinedReplica, PageAction, ReplicaActor, Seed};

/// The number of the first element whose opening tag carries all of `marks`.
fn element(html: &str, marks: &[&str]) -> Option<u32> {
    html.split('<').find_map(|tag| {
        let open = tag.split('>').next()?;
        if !marks.iter().all(|mark| open.contains(mark)) {
            return None;
        }
        let number = open.split("data-p=\"").nth(1)?.split('"').next()?;
        number.parse().ok()
    })
}

/// The number of the button whose words are `words`, which a press on them lands on.
fn button_saying(html: &str, words: &str) -> Option<u32> {
    let at = html.find(&format!(">{words}<"))?;
    let before = html.get(..at)?;
    let open = before.get(before.rfind("<button")?..)?.split('>').next()?;
    if !open.contains("data-a=\"activate\"") {
        return None;
    }
    open.split("data-p=\"")
        .nth(1)?
        .split('"')
        .next()?
        .parse()
        .ok()
}

#[test]
fn the_assistants_page_opens_the_persons_first_conversation_in_chat() {
    let apps = catalogue();
    let person = MemberId::person("ab".repeat(32));
    let page = |id| {
        assert!(
            apps.get(id)
                .expect("the server registers the app")
                .keeps_workspace_resource(),
            "the host must be able to serve {id} at its workspace URL"
        );
        let app = ConfinedReplica::open_page(
            &apps,
            (id, WORKSPACE),
            &person,
            &ReplicaActor::Owner,
            Seed::New,
        )
        .expect("the workspace app opens as a page");
        let _ = app.page_show(None).expect("the app asks for what it reads");
        for wanted in app.wanted_resources() {
            app.workspace_answered(&wanted, ui::resource::WorkspaceAnswer::HoldsNone)
                .expect("the new workspace answers");
        }
        app
    };
    let assistant = page(ui::apps::ASSISTANT);
    let (_, _, before) = assistant
        .page_show(None)
        .expect("the management page opens");
    let create =
        button_saying(&before, "Create assistant").expect("a new person can create an assistant");
    assistant
        .page_act(Some((create, None, PageAction::Activate)))
        .expect("create");
    let (_, _, after) = assistant
        .page_show(None)
        .expect("the page reflects the new agent");
    let button = after
        .split('<')
        .filter_map(|part| part.split('>').next())
        .find(|tag| tag.contains("data-open-app=\"so.tilo.chat\""))
        .expect("the new assistant links to its real Chat app");
    let target = button
        .split("data-open-item=\"")
        .nth(1)
        .and_then(|value| value.split('"').next())
        .expect("the link names the workspace agent");
    let (field, agent) = ui::tilo_resource::items::item_of_resource::<
        ui::tilo_workspace_agent::WorkspaceAgents,
    >(target)
    .expect("the destination names an agent");
    assert_eq!(field, "agents");
    let conversation =
        MemberId::direct_conversation([person.clone(), MemberId::agent(agent.to_string())]);
    let agents = ui::resource::workspace::document_id(ui::tilo_workspace_agent::KIND);
    assert!(
        assistant
            .take_document_mutations()
            .expect("the host can save the new assistant")
            .into_iter()
            .any(|(id, _)| id.as_str() == agents),
        "creation changed the workspace agents document"
    );
    let chat = page(ui::apps::CHAT);
    let (_, _, home) = chat.page_show(None).expect("Chat opens directly");
    assert!(
        element(
            &home,
            &["data-open-app=\"so.tilo.assistant\"", "data-a=\"activate\""]
        )
        .is_some(),
        "a member who starts in Chat can reach their assistant"
    );
    let (_, _, body) = chat
        .page_show(Some(target))
        .expect("Chat accepts the workspace agent as an item to open");
    let target = format!(
        "data-t=\"key:item:channel:{}\"",
        ui::composition::page::escape(&conversation)
    );
    assert!(
        element(&body, &[&target, "aria-selected=\"true\""]).is_some(),
        "the selected conversation must be the person's DM before either side has written"
    );
    assert!(
        element(
            &body,
            &[
                "data-open-app=\"so.tilo.assistant\"",
                "data-open-item=\"assistant.section:proposals\"",
                "data-a=\"activate\"",
            ]
        )
        .is_some(),
        "the signed-in person can review proposals from this DM"
    );
    let models = "settings.section:agent_models";
    for (app, html) in [("Assistant", &after), ("Chat", &body)] {
        assert!(
            element(
                html,
                &[
                    "data-open-app=\"so.tilo.settings\"",
                    &format!("data-open-item=\"{models}\""),
                    "data-a=\"activate\""
                ]
            )
            .is_some(),
            "{app} links to the person's model settings"
        );
    }
    post_on_page(&chat, &body, &person, &conversation);
    let settings = page(ui::apps::SETTINGS);
    let (_, _, body) = settings
        .page_show(Some(models))
        .expect("the settings page opens");
    let label = settings::categories::Category::AgentModels.label();
    let (_, _, row) = row_saying(&body, &label).expect("Settings offers Agent models");
    assert!(
        row.contains("aria-selected=\"true\""),
        "the link selects Agent models: {row}"
    );
}

fn post_on_page(chat: &ConfinedReplica, body: &str, person: &MemberId, conversation: &str) {
    let words = "Please help me prepare for tomorrow";
    let input = element(body, &["data-a=\"value\"", "data-t=\"chat.composer\""])
        .expect("the private chat offers an editable message");
    chat.page_act(Some((
        input,
        Some("chat.composer".into()),
        PageAction::Value(words.into()),
    )))
    .expect("the person can type a message");
    let (_, _, typed) = chat
        .page_show(None)
        .expect("the chat shows the person's draft");
    let send = element(&typed, &["data-a=\"activate\"", "data-t=\"chat.send\""])
        .expect("the written message can be sent");
    chat.page_act(Some((send, Some("chat.send".into()), PageAction::Activate)))
        .expect("the person can send it");
    for wanted in chat.wanted_resources() {
        chat.workspace_answered(&wanted, ui::resource::WorkspaceAnswer::HoldsNone)
            .expect("the workspace answers the chat's requests");
    }
    let (_, _, sent) = chat
        .page_show(None)
        .expect("the chat shows its conversation");
    assert!(
        sent.contains(words),
        "the transcript shows the sent message"
    );
    let part = chat::parts::part(0);
    assert!(
        chat.take_document_mutations()
            .expect("the host receives the chat edits")
            .iter()
            .any(|(id, _)| id == &part),
        "the first message is durable"
    );
    let saved = chat
        .read_resource(&part, chat::messages::read_part)
        .expect("the saved messages");
    let posted = saved
        .messages
        .iter()
        .map(|(_, message)| {
            (
                &message.author,
                message.channel.as_str(),
                message.text.as_str(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(posted, [(person, conversation, words)]);
}

#[test]
fn a_person_writes_an_issue_on_the_trackers_page() {
    let tracker = ConfinedReplica::open_page(
        &catalogue(),
        ("so.tilo.tasks", WORKSPACE),
        &MemberId::person("ada"),
        &ReplicaActor::Owner,
        Seed::New,
    )
    .expect("the tracker opens as a page");
    let _ = tracker.take_mutation();
    // A new workspace holds none of what the tracker keeps beside it, as its
    // serving devices answer a page's worker.
    let _ = tracker.page_show(None);
    for wanted in tracker.wanted_resources() {
        tracker
            .workspace_answered(&wanted, ui::resource::WorkspaceAnswer::HoldsNone)
            .expect("told");
    }
    let _ = tracker.take_document_mutations();

    let shown = std::time::Instant::now();
    let (title, css, body) = tracker.page_show(None).expect("the page shows");
    println!("page shown in {:?}, {} bytes", shown.elapsed(), body.len());
    assert_eq!(title, "Tasks");
    assert!(css.contains("--tilo-ground"), "the theme's colours: {css}");
    assert!(
        body.starts_with("<div data-p=\"0\""),
        "the page's body: {body}"
    );

    let field = element(
        &body,
        &["data-a=\"value\"", "placeholder=\"What needs doing\""],
    )
    .unwrap_or_else(|| panic!("the page offers a new issue's title as a field: {body}"));
    let typed = std::time::Instant::now();
    tracker
        .page_act(Some((field, None, PageAction::Value("Groceries".into()))))
        .expect("the person's typing is acted on");
    println!("typing answered in {:?}", typed.elapsed());

    let (_, _, body) = tracker.page_show(None).expect("the page shows");
    let new_issue = button_saying(&body, "New issue")
        .unwrap_or_else(|| panic!("the page offers a new issue: {body}"));
    let acted = std::time::Instant::now();
    let patches = tracker
        .page_act(Some((new_issue, None, PageAction::Activate)))
        .expect("the person's click is acted on");
    println!("click answered in {:?}", acted.elapsed());
    assert!(
        patches.iter().any(|(_, html)| html.contains("Groceries")),
        "the new issue's row says what was typed: {patches:?}"
    );
    let wrote = tracker.take_mutation().expect("the tracker's changes");
    let wrote_beside = tracker
        .take_document_mutations()
        .expect("the changes to its issues");
    assert!(
        wrote.is_some() || !wrote_beside.is_empty(),
        "what was typed is handed back to be saved"
    );

    // Nothing moved since: the page answers no change.
    assert!(
        tracker.page_act(None).expect("asked again").is_empty(),
        "an unchanged page sends nothing"
    );
}

/// The first page already has its address when the event's change is still
/// coming from the workspace. Its next patch must show the addressed day.
#[test]
fn a_page_reveals_the_event_that_arrives_after_its_first_frame() {
    let person = MemberId::person("ada");
    let mut calendar =
        crate::common::open(ui::apps::CALENDAR, &person, ReplicaActor::Owner, Seed::New)
            .expect("the calendar opens");
    let Some(collab_types::DocumentMutation::Snapshot(genesis)) = calendar.take_mutation().unwrap()
    else {
        panic!("a new calendar publishes its initial document");
    };
    let page = ConfinedReplica::open_page(
        &catalogue(),
        (ui::apps::CALENDAR, WORKSPACE),
        &person,
        &ReplicaActor::Owner,
        Seed::Existing {
            record: calendar.document_id().clone(),
            changes: vec![genesis],
        },
    )
    .expect("the calendar opens as a page");

    let id = "00000000-0000-4000-8000-000000000071";
    let target = format!("calendar.calendar/events/{id}");
    let date = "2077-09-08";
    let selected = [r#"type="date""#, r#"value="2077-09-08""#];
    let (_, _, before) = page.page_show(Some(&target)).expect("the first frame");
    assert!(element(&before, &selected).is_none());
    // Answer the same requests for side records as a new workspace, after
    // the browser has already asked to open the addressed event.
    for wanted in page.wanted_resources() {
        page.workspace_answered(&wanted, ui::resource::WorkspaceAnswer::HoldsNone)
            .expect("the workspace answers for Calendar's other records");
    }

    let new_event = calendar
        .survey()
        .into_iter()
        .find(|operation| operation.address.action == "new-event")
        .expect("Calendar publishes its event action");
    let args = serde_json::Map::from_iter([(
        "event".into(),
        serde_json::json!({
            "id": id, "title": "Future meeting",
            "start": format!("{date}T12:00"), "end": format!("{date}T13:00"),
        }),
    )]);
    calendar
        .invoke(&new_event.address, &args, false)
        .expect("another device adds the event");
    page.apply(
        &calendar
            .take_mutation()
            .unwrap()
            .expect("the event's change"),
    )
    .expect("the page takes in the workspace's record");
    let patches = page.page_act(None).expect("the browser follows the page");
    let (_, _, shown) = page.page_show(None).expect("Calendar's current frame");
    let dates = |html: &str| {
        html.split('<')
            .filter_map(|part| part.split_once('>').map(|(tag, _)| tag))
            .filter(|tag| tag.contains("type=\"date\""))
            .collect::<Vec<_>>()
            .join(" / ")
    };
    assert!(
        patches
            .iter()
            .any(|(_, html)| element(html, &selected).is_some())
            || element(&shown, &selected).is_some(),
        "Calendar moved to the event's date. Initially [{}], now [{}], patches {}. Targets {}",
        dates(&before),
        dates(&shown),
        patches.len(),
        shown
            .split('<')
            .filter_map(|part| part.split_once(" data-t=\""))
            .filter_map(|(_, tail)| tail.split('"').next())
            .take(30)
            .collect::<Vec<_>>()
            .join(" / ")
    );
}

/// The page's row saying `words`: the number of the element a press lands on (`page.mjs` finds
/// the nearest element carrying `data-a` over the words), the target it carries and its tag.
fn row_saying(html: &str, words: &str) -> Option<(u32, Option<String>, String)> {
    let at = html.find(&format!(">{words}<"))?;
    let before = html.get(..at)?;
    let start = before.rfind("<li")?;
    let open = before.get(start..)?.split('>').next()?;
    let number = open.split("data-p=\"").nth(1)?.split('"').next()?;
    let target = open
        .split("data-t=\"")
        .nth(1)
        .map(|rest| rest.split('"').next().unwrap_or_default().to_owned());
    Some((number.parse().ok()?, target, open.to_owned()))
}

/// A person in a browser opens Settings as a page and presses a category in the sidebar: that
/// category is the chosen one and its pane stands beside it, as a press in a window chooses it.
/// The category is one below the window's own height, which a page shows all the same.
#[test]
fn a_person_chooses_a_settings_category_on_the_page() {
    let label = settings::categories::Category::ExternalAccounts.label();
    let settings = ConfinedReplica::open_page(
        &catalogue(),
        ("so.tilo.settings", WORKSPACE),
        &MemberId::person("ada"),
        &ReplicaActor::Owner,
        Seed::New,
    )
    .expect("settings opens as a page");
    let _ = settings.take_mutation();
    let _ = settings.page_show(None);
    // A new workspace holds none of what the settings page keeps beside it, as its serving
    // devices answer a page's worker.
    for wanted in settings.wanted_resources() {
        settings
            .workspace_answered(&wanted, ui::resource::WorkspaceAnswer::HoldsNone)
            .expect("told");
    }
    let _ = settings.take_document_mutations();
    let (_, _, body) = settings.page_show(None).expect("the page shows");
    let (row, target, tag) = row_saying(&body, &label)
        .unwrap_or_else(|| panic!("the page's sidebar offers {label}: {body}"));
    assert!(
        tag.contains("data-a=\"activate\""),
        "a press on the row activates it: {tag}"
    );

    let patches = settings
        .page_act(Some((row, target, PageAction::Activate)))
        .expect("the person's press is acted on");
    let (_, _, after) = settings.page_show(None).expect("the page shows again");
    let (_, _, chosen) = row_saying(&after, &label).expect("the row stays");
    assert!(
        chosen.contains("aria-selected=\"true\""),
        "the pressed category is the chosen one: {chosen}; the page answered {patches:?}"
    );
}
