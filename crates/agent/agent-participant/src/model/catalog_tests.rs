//! How much of what the apps can do a plan is told, and where: in mode
//! `all` every app's reference after the instructions, in mode `relevant`
//! the line per app, the relevant apps in full, lookups and misses; the
//! prefix every plan of a task shares; and what a request costs, section by
//! section, for the campaign's agents and real apps.

use super::catalog::{INSTRUCTIONS_NOTE, relevant_apps};
use super::converse::PLANNER_INSTRUCTIONS;
use super::tests::{called, operation, results, route, scripted, text_of};
use super::*;
use crate::profile::Catalog;
use tilo_model_gateway::host_backend::protocol::{ModelRequest, Role};

const ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
const CHAT: &str = "so.tilo.chat";
const MAIL: &str = "so.tilo.mail";
const NOTES: &str = "so.tilo.notes";
const PDF: &str = "so.tilo.pdf-viewer";
const SHEETS: &str = "so.tilo.sheets";

/// Document operations `actions` of `resource`.
fn operations(resource: &str, actions: &[&str]) -> Vec<ActionDescriptor> {
    actions
        .iter()
        .map(|action| {
            operation(
                resource,
                action,
                tilo_actions::OperationKind::Document,
                vec![],
            )
        })
        .collect()
}

/// An agent granted Chat, Mail, Notes, the PDF viewer and Sheets, told
/// only the relevant ones: a Chat record open, the room's notes page
/// "Claims rules" and an untitled mailbox known.
fn casework() -> PlanContext {
    let chat = operations("chat.channel", &["post", "rename"]);
    let reference = vec![
        (CHAT.to_owned(), chat.clone()),
        (
            MAIL.to_owned(),
            operations("mail.mailbox", &["draft-message", "export-content"]),
        ),
        (
            NOTES.to_owned(),
            operations("notes.notebook", &["create-page", "append"]),
        ),
        (PDF.to_owned(), operations("pdf.document", &["look-page"])),
        (
            SHEETS.to_owned(),
            operations("sheets.workbook", &["set-cells", "upsert-item"]),
        ),
    ];
    PlanContext {
        open: vec![(route(CHAT, "c1"), chat)],
        known: vec![
            (route(NOTES, "n1"), Some("Claims rules".into())),
            (route(MAIL, "m1"), None),
        ],
        apps: reference.iter().map(|(app, _)| app.clone()).collect(),
        reference,
        room: ROOM.into(),
        catalog: Catalog::Relevant,
        app_names: [
            (CHAT, "Chat"),
            (MAIL, "Mail"),
            (NOTES, "Notes"),
            (PDF, "PDF Viewer"),
            (SHEETS, "Sheets"),
        ]
        .map(|(app, name)| (app.to_owned(), name.to_owned()))
        .to_vec(),
        ..PlanContext::default()
    }
}

const CASE_TASK: &str =
    "Check the Claims rules, update her row in the spreadsheet, then reply in her channel.";

fn first_user_text(request: &ModelRequest) -> String {
    request
        .messages
        .iter()
        .find(|message| message.role == Role::User)
        .map(text_of)
        .unwrap_or_default()
}

/// The text of each system message `request` opens with.
fn system_texts(request: &ModelRequest) -> Vec<String> {
    request
        .messages
        .iter()
        .take_while(|message| message.role == Role::System)
        .map(text_of)
        .collect()
}

fn tool_names(request: &ModelRequest) -> Vec<String> {
    request.tools.iter().map(|tool| tool.name.clone()).collect()
}

/// The first request a plan of `task` over `context` sends, and what the
/// plan was told of the apps.
fn first_request(task: &str, context: &PlanContext) -> (ModelRequest, CatalogUse) {
    let (mut planner, sent) = scripted(Vec::new(), false);
    let _ = planner.plan(task, context);
    let request = sent.lock().unwrap().first().cloned().unwrap();
    (request, planner.take_catalog().unwrap())
}

/// A plan that opens the room's mailbox and leaves the rest for then.
fn open_mailbox() -> serde_json::Value {
    serde_json::json!({"steps": [{"operation": "record.so.tilo.mail.m1#open", "arguments": {}}],
        "then": "draft her the email", "reply": "Opening the mailbox."})
}

/// Told only the relevant apps, a plan describes in full those the
/// request names (by a word for their records), those of a record it
/// names by title and those open, and lists every granted app in a line:
/// the operations of the others are not in the request.
#[test]
fn relevant_describes_the_named_and_open_apps_and_lists_the_rest() {
    let context = casework();
    assert_eq!(
        relevant_apps(CASE_TASK, &context),
        [CHAT, NOTES, SHEETS]
            .map(str::to_owned)
            .into_iter()
            .collect(),
    );
    let (request, used) = first_request(CASE_TASK, &context);
    // The instructions, ending with the line per app; the reference; then
    // the request.
    let [instructions, reference] = <[String; 2]>::try_from(system_texts(&request)).unwrap();
    assert!(instructions.starts_with(PLANNER_INSTRUCTIONS));
    assert!(instructions.contains(INSTRUCTIONS_NOTE));
    for app in [CHAT, MAIL, NOTES, PDF, SHEETS] {
        assert!(
            instructions.contains(&format!("\n- {app} (")),
            "{app} listed: {instructions}"
        );
    }
    assert!(
        instructions
            .contains("- so.tilo.mail (Mail): mail.mailbox — draft-message, export-content\n"),
        "{instructions}"
    );
    for described in [CHAT, NOTES, SHEETS] {
        assert!(
            reference.contains(&format!("## {described}\n")),
            "{reference}"
        );
    }
    for left in [MAIL, PDF] {
        assert!(!reference.contains(&format!("## {left}\n")), "{reference}");
    }
    let turn = first_user_text(&request);
    for text in [&instructions, &reference, &turn] {
        assert!(!text.contains("mail.mailbox#draft-message"), "{text}");
    }
    assert!(turn.starts_with("Operations published right now"), "{turn}");
    // The lookup alone: every operation fits its app's section, so none
    // is left to `describe_operation`.
    let tools = tool_names(&request);
    assert_eq!(
        tools.first().map(String::as_str),
        Some(tilo_agent::tools::LIST_OPERATIONS),
        "{tools:?}"
    );
    assert!(
        !tools.contains(&tilo_agent::tools::DESCRIBE_OPERATION.to_owned()),
        "{tools:?}"
    );
    assert_eq!(used.mode, Catalog::Relevant);
    assert_eq!(used.described, [CHAT, NOTES, SHEETS]);
    assert_eq!(used.listed, [MAIL, PDF]);
}

/// An app is relevant by what it calls itself, by a tag a record of it
/// holds (a channel the request names), by the record a step would start
/// and, for an item's agent, always; an app the request does not touch is
/// not.
#[test]
fn an_app_is_relevant_by_its_name_a_tag_or_a_new_record() {
    let mut context = casework();
    context.open.clear();
    context.known.clear();
    assert!(relevant_apps("Look at her scans in the pdf viewer", &context).contains(PDF));
    assert!(relevant_apps("open the scan reader", &context).is_empty());
    context.app_names.retain(|(app, _)| app != PDF);
    context.app_names.push((PDF.into(), "Scan Reader".into()));
    assert!(relevant_apps("open the scan reader", &context).contains(PDF));
    assert!(relevant_apps("Post the code in #logins", &context).is_empty());
    context.searchable.push(crate::search::SearchDoc {
        route: route(CHAT, "c1"),
        title: None,
        text: "#general\n#logins\nLR-2041 signed in".into(),
        changed: None,
        open: false,
    });
    assert_eq!(
        relevant_apps("Post the code in #logins.", &context),
        [CHAT.to_owned()].into_iter().collect()
    );
    context.previews.push((MAIL.into(), Vec::new()));
    assert!(relevant_apps("Draft her a reply", &context).contains(MAIL));
    context.role = crate::planner::Role::Item {
        note: String::new(),
    };
    assert_eq!(relevant_apps("anything", &context).len(), 5);
}

/// `list_operations` for an app left out answers with its operations in
/// full, says how to use them while none of its documents is open, and is
/// counted as looked up; the plan after it opens the room's mailbox, and
/// the turn after the lookup sends the same prefix.
#[test]
fn a_lookup_brings_in_an_apps_operations() {
    let context = casework();
    let (mut planner, sent) = scripted(
        vec![
            called(
                "l1",
                tilo_agent::tools::LIST_OPERATIONS,
                &serde_json::json!({"application": "Mail"}),
            ),
            called("p1", tilo_agent::tools::PLAN, &open_mailbox()),
        ],
        false,
    );
    let steps = planner.plan(CASE_TASK, &context).unwrap();
    assert_eq!(steps[0].action, StepAction::Open);
    assert_eq!(steps[0].record.as_deref(), Some("m1"));
    let sent = sent.lock().unwrap().clone();
    let answer = results(&sent[1]).join("\n");
    assert!(
        answer.contains("What so.tilo.mail can do")
            && answer.contains("mail.mailbox#draft-message — draft-message")
            && answer.contains("None of so.tilo.mail's documents is open")
            && answer.contains("record.so.tilo.mail.m1#open"),
        "{answer}"
    );
    assert_eq!(sent[0].tools, sent[1].tools);
    assert_eq!(
        sent[0].messages.as_slice(),
        sent[1].messages.get(..sent[0].messages.len()).unwrap()
    );
    let used = planner.take_catalog().unwrap();
    assert_eq!((used.looked_up, used.misses), (vec![MAIL.to_owned()], 0));
}

/// A plan naming an operation of an app it was not described is refused
/// as any unshown id is, with that app's operations in full for its next
/// turn, and counted as a miss; one naming an offered operation of an app
/// with no reference runs, counted too.
#[test]
fn a_plan_naming_an_app_it_was_not_told_works_and_is_counted() {
    let draft = serde_json::json!({"steps": [{"operation": "mail.mailbox#draft-message",
        "arguments": {}}], "reply": "Drafted."});
    let (mut planner, sent) = scripted(
        vec![
            called("p1", tilo_agent::tools::PLAN, &draft),
            called("p2", tilo_agent::tools::PLAN, &open_mailbox()),
        ],
        false,
    );
    let steps = planner.plan(CASE_TASK, &casework()).unwrap();
    assert_eq!(steps[0].record.as_deref(), Some("m1"));
    let refusal = results(&sent.lock().unwrap()[1]).join("\n");
    assert!(
        refusal.contains("which no listing in this conversation returned")
            && refusal.contains("The plan names operations of so.tilo.mail")
            && refusal.contains("mail.mailbox#draft-message — draft-message"),
        "{refusal}"
    );
    let used = planner.take_catalog().unwrap();
    assert_eq!((used.looked_up, used.misses), (vec![MAIL.to_owned()], 1));

    // A mailbox open whose app could not be previewed: its operations are
    // offered, with no reference to describe them.
    let mut context = casework();
    context.reference.retain(|(app, _)| app != MAIL);
    context.open.push((
        route(MAIL, "m1"),
        operations("mail.mailbox", &["draft-message"]),
    ));
    let (mut planner, _) = scripted(vec![called("p1", tilo_agent::tools::PLAN, &draft)], false);
    let steps = planner.plan(CASE_TASK, &context).unwrap();
    assert!(matches!(
        &steps[0].action,
        StepAction::Invoke { address, .. } if address.action == "draft-message"
    ));
    let used = planner.take_catalog().unwrap();
    assert_eq!((used.looked_up.len(), used.misses), (0, 1));
}

/// Every plan of a task opens with the same bytes: the tools and the
/// instructions (told only the relevant apps, with the line per app),
/// whatever the system message after them describes; told every app, the
/// reference after them too.
#[test]
fn the_stable_prefix_is_the_same_across_a_tasks_plans() {
    for catalog in [Catalog::Relevant, Catalog::All] {
        let mut context = casework();
        context.catalog = catalog;
        let reply = "Reply briefly in her channel.";
        let (before, _) = first_request(reply, &context);
        // A step of the task opened the workbook since.
        context.open.push((
            route(SHEETS, "w1"),
            operations("sheets.workbook", &["set-cells"]),
        ));
        let (after, _) = first_request(reply, &context);
        assert_eq!(before.tools, after.tools);
        let (before, after) = (system_texts(&before), system_texts(&after));
        assert_eq!(before[0], after[0]);
        if catalog == Catalog::Relevant {
            assert!(before[0].ends_with(&catalog::index(&context)));
            assert!(
                !before[1].contains("## so.tilo.sheets") && after[1].contains("## so.tilo.sheets")
            );
        } else {
            assert_eq!(before, after);
        }
    }
}

/// In mode `all` the instructions are a system message of their own,
/// the same for every agent, and the reference of every app the context
/// offers is the next; the first turn starts with what is published. No
/// lookups, and nothing of the app names the relevant mode uses.
#[test]
fn mode_all_tells_the_reference_as_system_content() {
    let mut context = casework();
    context.catalog = Catalog::All;
    let (named, used) = first_request(CASE_TASK, &context);
    context.app_names.clear();
    let (unnamed, _) = first_request(CASE_TASK, &context);
    assert_eq!(
        serde_json::to_string(&named).unwrap(),
        serde_json::to_string(&unnamed).unwrap()
    );
    let [instructions, reference] = <[String; 2]>::try_from(system_texts(&named)).unwrap();
    assert_eq!(instructions, PLANNER_INSTRUCTIONS);
    assert!(reference.starts_with(REFERENCE_INTRO), "{reference}");
    for app in [CHAT, MAIL, NOTES, PDF, SHEETS] {
        assert!(reference.contains(&format!("## {app}\n")), "{reference}");
    }
    let turn = first_user_text(&named);
    assert!(turn.starts_with("Operations published right now"), "{turn}");
    assert!(!turn.contains(REFERENCE_INTRO), "{turn}");
    assert!(!tool_names(&named).contains(&tilo_agent::tools::LIST_OPERATIONS.to_owned()));
    assert_eq!(used.mode, Catalog::All);
    assert_eq!(used.described.len(), 5);
    assert!(used.listed.is_empty());
}

/// An operation the reference describes is named in the list of what is
/// published without its signature again (`<resource># a, b`), and may be
/// planned; one whose record publishes other parameters or choices than
/// the reference shows (its own sheets) is listed in full.
#[test]
fn the_published_list_does_not_repeat_what_the_reference_describes() {
    use tilo_actions::{ActionParam, ParamType};
    for catalog in [Catalog::All, Catalog::Relevant] {
        let mut context = casework();
        context.catalog = catalog;
        let sheet = |choices: &[&str]| {
            let mut set = operation(
                "sheets.workbook",
                "set-cells",
                tilo_actions::OperationKind::Document,
                vec![
                    ActionParam::required("sheet", ParamType::Text, "The sheet")
                        .with_choices(choices.iter().map(|choice| (*choice).to_owned())),
                ],
            );
            set.description = None;
            set
        };
        context.reference[4].1 = vec![sheet(&[])];
        context
            .open
            .push((route(SHEETS, "w1"), vec![sheet(&["Cases"])]));
        let (request, _) = first_request(CASE_TASK, &context);
        let turn = first_user_text(&request);
        assert!(turn.contains("  chat.channel# post, rename\n"), "{turn}");
        assert!(!turn.contains("chat.channel#post — "), "{turn}");
        assert!(
            turn.contains("  sheets.workbook#set-cells — set-cells {sheet: text [Cases]}"),
            "{turn}"
        );
        assert!(turn.contains("names operations whose parameters are described"));
    }
    // A plan naming a compacted operation runs.
    let post = serde_json::json!({"steps": [{"operation": "chat.channel#post",
        "arguments": {}}], "reply": "Posted."});
    let (mut planner, _) = scripted(vec![called("p1", tilo_agent::tools::PLAN, &post)], false);
    let steps = planner.plan(CASE_TASK, &casework()).unwrap();
    assert_eq!(steps[0].record.as_deref(), Some("c1"));
}

/// What a new record of `app` publishes.
fn survey(app: &str) -> Vec<ActionDescriptor> {
    crate::worker::ConfinedReplica::open(
        &crate::AppCatalogue::native(),
        app,
        ROOM,
        "measure",
        &crate::ReplicaActor::Agent,
        crate::Seed::New,
    )
    .unwrap()
    .survey()
}

/// The context of an agent granted `apps` planning `task`, with a record
/// of each of `open` open, as the participant builds it: in mode `all`,
/// every app's reference for an agent granted at most four, else those
/// open or named; in mode `relevant`, every app's, with their names.
fn agent_context(apps: &[&str], open: &[&str], task: &str, catalog: Catalog) -> PlanContext {
    let surveys: Vec<(String, Vec<ActionDescriptor>)> = apps
        .iter()
        .map(|app| ((*app).to_owned(), survey(app)))
        .collect();
    let native = crate::AppCatalogue::native();
    PlanContext {
        open: surveys
            .iter()
            .filter(|(app, _)| open.contains(&app.as_str()))
            .map(|(app, descriptors)| (route(app, &format!("{app}-1")), descriptors.clone()))
            .collect(),
        apps: apps.iter().map(|app| (*app).to_owned()).collect(),
        reference: surveys
            .iter()
            .filter(|(app, _)| {
                catalog == Catalog::Relevant
                    || apps.len() <= 4
                    || open.contains(&app.as_str())
                    || task_names_app(task, app)
            })
            .cloned()
            .collect(),
        room: ROOM.into(),
        catalog,
        app_names: if catalog == Catalog::Relevant {
            apps.iter()
                .filter_map(|app| Some(((*app).to_owned(), native.get(app)?.title.to_string())))
                .collect()
        } else {
            Vec::new()
        },
        ..PlanContext::default()
    }
}

/// A request's characters by section: the instructions, the tools, and the
/// first turn's line per app, reference, published operations, state and
/// request.
fn sections(request: &ModelRequest) -> Vec<(&'static str, usize)> {
    let systems = system_texts(request);
    let system = systems.first().cloned().unwrap_or_default();
    let instructions = system.find("The apps you may use").unwrap_or(system.len());
    let first = first_user_text(request);
    let at = |marker: &str| first.find(marker).unwrap_or(first.len());
    let published = at("Operations published right now:");
    let task = at("The instruction asks for:");
    let state = at(SNAPSHOT_INTRO).min(task);
    let reference = systems.get(1).map_or(published, String::len);
    vec![
        ("system", instructions),
        ("index", system.len().saturating_sub(instructions)),
        (
            "tools",
            serde_json::to_string(&request.tools)
                .unwrap()
                .chars()
                .count(),
        ),
        ("reference", reference),
        ("published", state.saturating_sub(published)),
        ("state", task.saturating_sub(state)),
        ("request", first.len().saturating_sub(task)),
    ]
}

/// The r2-casework case handler's request, shortened.
const CASEWORK: &str = "You handle Harbour Mutual gadget claims. Lena Hoffmann is asking about \
    her claim on WhatsApp: her conversation is the \"WhatsApp: Lena Hoffmann (+49 151 2345 \
    6789)\" channel in Chat. Read it all, then check her case on the \"Cases\" sheet of the \
    Claims admin workbook in Sheets, her documents in the PDF viewer and our claims rules in \
    Notes. Then update her row on \"Cases\", draft one email to her in Mail and reply briefly \
    in her WhatsApp channel.";

/// The r4-promo-cut reviewer's request, shortened.
const REVIEW: &str = "You review the Harbour Days promo for the studio and deal with the \
    client by email. Check v1 in the Video Editor project \"Harbour Days promo\" against the \
    approved brief in Notes by looking at it. If his email is not in Mail yet, wait for it. \
    Then add a page \"Review v1\" to the Notes notebook.";

/// Every request of the campaign's agents over real apps, measured in both
/// modes: printed per section with a hash of the whole, so a change to
/// what is sent shows. Told only the relevant apps, a request about fewer
/// than all of them is smaller.
#[test]
fn the_planning_prompt_measured_per_section() {
    let video = "so.tilo.video-editor";
    let handler = [CHAT, SHEETS, PDF, NOTES, MAIL];
    let rhea = [video, NOTES, MAIL];
    let reply = "Reply briefly in her WhatsApp channel in Chat that her update is on its way.";
    let cases = [
        (
            "r2 Cass, every app open",
            &handler[..],
            &handler[..],
            CASEWORK,
        ),
        (
            "r2 Cass, Chat open, a reply",
            &handler[..],
            &[CHAT][..],
            reply,
        ),
        (
            "r4 Rhea, the review",
            &rhea[..],
            &[video, NOTES][..],
            REVIEW,
        ),
        (
            "r4 Rhea, a page",
            &rhea[..],
            &[NOTES][..],
            "Add a page \"Review v1\" to the Notes notebook.",
        ),
    ];
    for (name, apps, open, task) in cases {
        let mut totals = Vec::new();
        for catalog in [Catalog::All, Catalog::Relevant] {
            let context = agent_context(apps, open, task, catalog);
            let (request, _) = first_request(task, &context);
            let measured = sections(&request);
            let total: usize = measured.iter().map(|(_, size)| size).sum();
            let hash = blake3::hash(serde_json::to_string(&request).unwrap().as_bytes());
            eprintln!(
                "{name}, {}: {total} chars (~{} tokens) {measured:?} hash {}",
                catalog.name(),
                total.div_ceil(4),
                hash.to_hex().get(..16).unwrap_or_default()
            );
            totals.push(total);
        }
        if name.ends_with("a page") {
            assert!(totals[1] < totals[0], "{name}: {totals:?}");
        }
    }
}
