use super::*;
use crate::surface::ResourceFacts;
use tilo_actions::door::{ActionAddress, ActionDescriptor};
use tilo_actions::{Collects, ExecutionLocus, InputSpec};

const STEPS: usize = 4;

#[test]
fn typed_tools_are_stable_and_validate_whole_batches() {
    let definitions = typed_definitions(STEPS);
    assert_eq!(definitions[3].name, PLAN);
    assert!(
        definitions[3].input_schema["properties"]["steps"]["items"]["properties"]["operation"]
            .get("enum")
            .is_none()
    );
    assert_eq!(
        serde_json::to_value(&definitions).unwrap(),
        super::definitions(STEPS)
    );
    let loops = typed_loop_definitions();
    assert_eq!(serde_json::to_value(&loops).unwrap(), loop_definitions());
    assert_eq!(
        loops[3].input_schema["properties"][INVOKE_STANDALONE]["type"],
        "boolean"
    );
    let call = |id: &str, name: &str, input: Value| ToolCall {
        id: id.into(),
        name: name.into(),
        input,
        extra_content: None,
    };
    assert!(
        validate_plan_calls(&[
            call("a", LIST_APPLICATIONS, json!({})),
            call("b", PLAN, json!({}))
        ])
        .is_err()
    );
    assert!(validate_plan_calls(&[call("a", PLAN, json!({}))]).is_ok());
    assert!(
        validate_loop_calls(&[
            call("a", LIST_APPLICATIONS, json!({})),
            call("a", LIST_OPERATIONS, json!({}))
        ])
        .is_err()
    );
    assert!(
        validate_loop_calls(&[call(
            "a",
            INVOKE,
            json!({"operation": "shown", "parameters": {}, "standalone": true})
        )])
        .is_ok()
    );
    assert!(
        validate_loop_calls(&[
            call("a", INVOKE, json!({"operation": "shown", "parameters": {}})),
            call("b", LIST_OPERATIONS, json!({"application": false}))
        ])
        .is_err()
    );
    // Every tool the loop offers is accepted as its schema says.
    assert!(
        validate_loop_calls(&[call(
            "a",
            crate::pages::READ_RESULT,
            json!({"result": "r1", "from": 0})
        )])
        .is_ok()
    );
    assert!(
        validate_loop_calls(&[call("a", crate::pages::READ_RESULT, json!({"from": 0}))]).is_err()
    );
    assert!(validate_loop_calls(&[call("a", "unoffered", json!({}))]).is_err());
    assert!(
        validate_loop_calls(&[call(
            "a",
            INVOKE,
            json!({"operation": "shown", "parameters": {},
                   "progress": {"done": 3, "of": 8, "item": "reviewed Jane Doe"}})
        )])
        .is_ok()
    );
}

#[test]
fn refused_results_name_what_is_wrong_and_answer_every_call() {
    let call = |id: &str, name: &str, input: Value| ToolCall {
        id: id.into(),
        name: name.into(),
        input,
        extra_content: None,
    };
    // A batch the loop can run is not refused.
    let good = call("a", INVOKE, json!({"operation": "shown", "parameters": {}}));
    assert_eq!(
        refused_results(std::slice::from_ref(&good)),
        Some(Vec::new())
    );
    // A missing argument names the argument and what the call needs.
    let missing = call("b", INVOKE, json!({"parameters": {}}));
    let refused = refused_results(std::slice::from_ref(&missing)).unwrap();
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0].0, "b");
    assert!(refused[0].1.contains("missing operation"), "{refused:?}");
    assert!(
        refused[0].1.contains("required arguments are: operation"),
        "{refused:?}"
    );
    // One malformed call refuses the whole turn: every call is answered,
    // the sound ones told the turn did not run them.
    let turned_down = refused_results(&[good, missing]).unwrap();
    assert_eq!(turned_down.len(), 2);
    assert!(
        turned_down[0]
            .1
            .contains("another call in it was malformed")
    );
    // Without an id to answer, nothing can be fed back.
    let unnamed = call("", "plan", json!({}));
    assert_eq!(refused_results(&[unnamed]), None);
}

pub(super) fn offer(
    app: &'static str,
    resource: &str,
    kind: &str,
    action: &str,
    label: &str,
    input: InputSpec,
) -> Offer {
    let params = matches!(&input, InputSpec::Text { .. })
        .then(|| {
            vec![tilo_actions::ActionParam::required(
                "value",
                tilo_actions::ParamType::Text,
                "value",
            )]
        })
        .unwrap_or_default();
    Offer {
        app: app.to_string(),
        broker_peer_id: None,
        broker_provider: None,
        descriptor: ActionDescriptor {
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: ActionAddress::new(resource, action),
            resource_kind: kind.to_string(),
            resource_label: "a thing".to_string(),
            action_label: label.to_string(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            locus: ExecutionLocus::ClientLocal,
            kind: tilo_actions::OperationKind::Document,
            contract: tilo_actions::ActionContract::default(),
            params,
            input,
            view: None,
        },
    }
}

/// A workspace with the word processor open and holding operations, plus a
/// resource `describe_operation` has a fact but no action for.
fn workspace() -> Catalogue {
    Catalogue {
        held: vec!["word-processor", "files"],
        offers: vec![
            offer(
                "word-processor",
                "word-processor.document",
                "word_processor.document",
                "insert-table",
                "Insert table",
                InputSpec::None,
            ),
            offer(
                "word-processor",
                "word-processor.document",
                "word_processor.document",
                "place-image",
                "Place image",
                InputSpec::Text {
                    prompt: "The picture to place".to_string(),
                    placeholder: String::new(),
                    collects: Collects::Content,
                },
            ),
            offer(
                "files",
                "files.entry:/home/q/harbour.png",
                "files.entry",
                "rename",
                "Rename",
                InputSpec::Text {
                    prompt: "The new name".to_string(),
                    placeholder: "notes.md".to_string(),
                    collects: Collects::Words,
                },
            ),
        ],
        facts: vec![ResourceFacts {
            id: "files.entry:/home/q/harbour.png".to_string(),
            kind: "files.entry".to_string(),
            label: "harbour.png".to_string(),
            notes: vec!["512 x 320 pixels".to_string()],
        }],
    }
}

/// An id copied without its `broker:<n>:` routing prefix names the one
/// shown operation with that address; an ambiguous or unshown one is
/// refused with the listed ids for the same action.
#[test]
fn an_id_without_its_routing_prefix_names_the_one_shown_operation() {
    let mut revealed = Revealed::new();
    revealed.reveal("broker:3:word-processor.document#append-paragraph");
    revealed.reveal("broker:4:chat.channel:general#post");
    assert_eq!(
        revealed.resolve("word-processor.document#append-paragraph"),
        Ok("broker:3:word-processor.document#append-paragraph".to_owned())
    );
    assert_eq!(
        revealed.resolve("broker:9:chat.channel:general#post"),
        Ok("broker:4:chat.channel:general#post".to_owned())
    );
    let unshown = revealed.resolve("word-processor.other#append-paragraph");
    let error = unshown.unwrap_err();
    assert!(error.contains("is not an operation any listing"), "{error}");
    assert!(
        error.contains("broker:3:word-processor.document#append-paragraph"),
        "suggests the shown id for the same action: {error}"
    );
    revealed.reveal("broker:5:word-processor.document#append-paragraph");
    assert!(
        revealed
            .resolve("word-processor.document#append-paragraph")
            .is_err(),
        "two peers publish that address: ambiguous"
    );
    assert!(revealed.resolve(plan::NOTHING_FITS).is_err());
}

/// The first turn's summary always shows how to open a closed
/// application, and gives every application a share however many
/// operations one of them publishes.
#[test]
fn the_summary_keeps_opens_and_every_application_in_view() {
    let mut catalogue = workspace();
    for n in 0..50 {
        catalogue.offers.push(offer(
            "word-processor",
            "word-processor.document",
            "word_processor.document",
            &format!("op-{n}"),
            "Something",
            InputSpec::None,
        ));
    }
    let mut open = offer(
        "desktop",
        "desktop:app:so.tilo.notes",
        "desktop.application",
        "open",
        "Open Notes",
        InputSpec::None,
    );
    open.descriptor.kind = tilo_actions::OperationKind::Effect;
    catalogue.offers.push(open);
    let mut revealed = Revealed::new();
    let summary = catalogue_summary(&catalogue, &mut revealed, 10);
    assert!(
        summary.contains("desktop:app:so.tilo.notes#open — Open Notes (opens so.tilo.notes"),
        "{summary}"
    );
    assert!(
        summary.contains("files.entry:/home/q/harbour.png#rename"),
        "{summary}"
    );
    assert!(summary.contains("{value: text}"), "{summary}");
    assert!(summary.contains("In word-processor:"), "{summary}");
    let listed = operations(
        &json!({"application": "so.tilo.notes"}),
        &catalogue,
        &mut revealed,
    );
    assert!(
        listed.contains("desktop:app:so.tilo.notes#open"),
        "{listed}"
    );
    let none = operations(
        &json!({"application": "nowhere"}),
        &catalogue,
        &mut revealed,
    );
    assert!(
        none.contains("one of desktop, files, word-processor"),
        "{none}"
    );
}

/// The opens of everything installed, host applications included, do
/// not crowd what a running application can do out of the summary:
/// Notes' page stays listed however many applications could be opened,
/// and Tilo's opens come before the host's.
#[test]
fn opens_do_not_crowd_out_what_is_running() {
    let mut catalogue = Catalogue {
        offers: vec![offer(
            "so.tilo.notes",
            "notes.page:1",
            "notes.page",
            "read",
            "Read the page",
            InputSpec::None,
        )],
        facts: Vec::new(),
        held: Vec::new(),
    };
    let open = |app: &str| {
        let mut open = offer(
            "desktop",
            &format!("desktop:app:{app}"),
            "desktop.application",
            "open",
            "Open",
            InputSpec::None,
        );
        open.descriptor.kind = tilo_actions::OperationKind::Effect;
        open
    };
    for n in 0..40 {
        catalogue.offers.push(open(&format!("host:app-{n}")));
    }
    catalogue.offers.push(open("so.tilo.notes"));
    let summary = catalogue_summary(&catalogue, &mut Revealed::new(), 10);
    assert!(summary.contains("notes.page:1#read"), "{summary}");
    assert!(
        summary.contains("desktop:app:so.tilo.notes#open"),
        "{summary}"
    );
    assert!(summary.contains("list_applications"), "{summary}");
}

/// A request naming a Tilo application by its title names it; a host
/// application's everyday title ("Web") does not.
#[test]
fn a_host_application_is_never_named_by_a_request() {
    let open = |app: &str, title: &str| {
        let mut open = offer(
            "desktop",
            &format!("desktop:app:{app}"),
            "desktop.application",
            "open",
            "Open",
            InputSpec::None,
        );
        open.descriptor.resource_label = title.to_owned();
        open
    };
    let catalogue = Catalogue {
        offers: vec![
            open("host:org.gnome.Epiphany", "Web"),
            open("so.tilo.word-processor", "Writer"),
        ],
        facts: Vec::new(),
        held: Vec::new(),
    };
    let request = "find it on the web and put it in writer";
    assert_eq!(
        named_applications(&catalogue, request),
        vec![("so.tilo.word-processor", "Writer")]
    );
}

/// An object or array argument a model wrote as its JSON text is read
/// back as the object or array; text parameters, and text that is not
/// the declared kind of JSON, stay text.
#[test]
fn json_text_for_an_object_or_array_argument_is_decoded() {
    let mut replace = offer(
        "notes",
        "notes.page:1/body",
        "tilo.rich-text",
        "replace-range",
        "Replace text at a place",
        InputSpec::None,
    );
    replace.descriptor.params = vec![
        tilo_actions::ActionParam::required("at", tilo_actions::ParamType::Object, "where")
            .as_place(),
        tilo_actions::ActionParam::optional("marks", tilo_actions::ParamType::Array, "marks"),
        tilo_actions::ActionParam::required("text", tilo_actions::ParamType::Text, "text"),
    ];
    let mut arguments = json!({
        "at": "{\"item\":\"b1\",\"start\":0,\"end\":39}",
        "marks": "[\"link\"]",
        "text": "{\"looks\":\"like json\"}",
    })
    .as_object()
    .cloned()
    .unwrap();
    decode_structured_arguments(&replace, &mut arguments);
    assert_eq!(
        arguments["at"],
        json!({"item": "b1", "start": 0, "end": 39})
    );
    assert_eq!(arguments["marks"], json!(["link"]));
    assert_eq!(arguments["text"], json!("{\"looks\":\"like json\"}"));
    let mut wrong = json!({"at": "[1, 2]"}).as_object().cloned().unwrap();
    decode_structured_arguments(&replace, &mut wrong);
    assert_eq!(wrong["at"], json!("[1, 2]"));
}

/// A place whose fields a model wrote beside the other arguments is
/// gathered back into the place parameter, a range or an insertion.
#[test]
fn a_place_written_flat_is_gathered_into_its_parameter() {
    let mut replace = offer(
        "notes",
        "notes.page:1/body",
        "tilo.rich-text",
        "replace-range",
        "Replace text at a place",
        InputSpec::None,
    );
    replace.descriptor.params = vec![
        tilo_actions::ActionParam::required("at", tilo_actions::ParamType::Object, "where")
            .as_place(),
        tilo_actions::ActionParam::required("text", tilo_actions::ParamType::Text, "text"),
    ];
    let flat = |value: Value| value.as_object().cloned().unwrap();
    let mut range = flat(json!({"at": 1, "item": "b1", "start": 0, "end": 39, "text": "x"}));
    decode_structured_arguments(&replace, &mut range);
    assert_eq!(
        Value::Object(range),
        json!({"at": {"item": "b1", "start": 0, "end": 39}, "text": "x"})
    );
    let mut insert = flat(json!({"at": 4, "item": "b1", "text": "x"}));
    decode_structured_arguments(&replace, &mut insert);
    assert_eq!(
        Value::Object(insert),
        json!({"at": {"item": "b1", "at": 4}, "text": "x"})
    );
    let mut whole = flat(json!({"at": {"item": "b1", "at": 4}, "item": "b2", "text": "x"}));
    decode_structured_arguments(&replace, &mut whole);
    assert_eq!(
        whole["at"],
        json!({"item": "b1", "at": 4}),
        "a given place stays"
    );
}

/// An open next to an application that already publishes operations
/// says so: opening again makes a second window.
#[test]
fn an_open_of_an_already_open_application_says_so() {
    let mut catalogue = workspace();
    let mut open = offer(
        "desktop",
        "desktop:app:word-processor",
        "desktop.application",
        "open",
        "Open Writer",
        InputSpec::None,
    );
    open.descriptor.kind = tilo_actions::OperationKind::Effect;
    catalogue.offers.push(open);
    let summary = catalogue_summary(&catalogue, &mut Revealed::new(), MOST_SUMMARISED);
    assert!(
        summary.contains("word-processor is already open"),
        "{summary}"
    );
}

/// An operation the person is asked about first is listed, marked as
/// such everywhere the model meets it, and the model is told it waits.
#[test]
fn an_operation_needing_confirmation_is_listed_and_marked() {
    let mut catalogue = workspace();
    let mut send = offer(
        "mail",
        "mail.inbox",
        "mail.mailbox",
        "send_message",
        "Send message",
        InputSpec::None,
    );
    send.descriptor.contract.confirmation = tilo_actions::ConfirmationPolicy::Confirm;
    catalogue.offers.push(send.clone());
    let mut revealed = Revealed::new();
    let summary = catalogue_summary(&catalogue, &mut revealed, MOST_SUMMARISED);
    let line = summary
        .lines()
        .find(|line| line.contains("Send message"))
        .unwrap_or_default();
    assert!(line.contains("needs confirmation"), "{summary}");
    let insert = summary
        .lines()
        .find(|line| line.contains("Insert table"))
        .unwrap_or_default();
    assert!(!insert.contains("needs confirmation"), "{summary}");
    let listed = operations(&json!({"application": "mail"}), &catalogue, &mut revealed);
    assert!(listed.contains("needs confirmation"), "{listed}");
    let described = describe(&json!({"operation": send.id()}), &catalogue, &mut revealed);
    assert!(
        described.contains("needs confirmation: your step waits"),
        "{described}"
    );
    assert!(!ARGUMENTS_DESCRIPTION.contains("include `confirm: true`"));
}

/// A refused argument says what the operation takes and how to pass it.
#[test]
fn a_parameter_hint_names_each_parameter_and_an_example() {
    let catalogue = workspace();
    let rename = &catalogue.offers[2];
    let hint = parameter_hint(rename);
    assert!(hint.contains("`value`: text (required)"), "{hint}");
    assert!(hint.contains("\"parameters\": {\"value\": …}"), "{hint}");
}

/// `edit-text` names the type's editable fields in its `field` parameter, so
/// `describe_operation` and the refusal hint show an agent the field to set instead of its
/// guessing at an internal record id.
#[test]
fn describe_operation_names_an_edit_verbs_fields() {
    let mut catalogue = workspace();
    let mut edit = offer(
        "notes",
        "notes.note:n1",
        "tl.so.tilo.lang.notes.Note",
        "edit-text",
        "Edit a text field",
        InputSpec::None,
    );
    edit.descriptor.params = vec![
        tilo_actions::ActionParam::required(
            "field",
            tilo_actions::ParamType::Text,
            "The field's name, one of: title, body",
        ),
        tilo_actions::ActionParam::required(
            "value",
            tilo_actions::ParamType::Text,
            "The new value",
        ),
    ];
    catalogue.offers.push(edit.clone());
    let mut revealed = Revealed::new();
    let described = describe(&json!({"operation": edit.id()}), &catalogue, &mut revealed);
    assert!(
        described.contains("field: text (required)"),
        "the field parameter is listed: {described}"
    );
    assert!(
        described.contains("one of: title, body"),
        "the fields an agent may set, by their names: {described}"
    );
    // A refused invoke's hint names them too, so the next attempt needs no describe turn.
    let hint = parameter_hint(&edit);
    assert!(hint.contains("one of: title, body"), "{hint}");
}

#[test]
fn discovery_answers_list_and_describe_only_what_the_workspace_offers() {
    {
        let mut revealed = Revealed::new();
        let out = operations(
            &json!({"kind": "word_processor.document"}),
            &workspace(),
            &mut revealed,
        );
        assert!(out.contains("insert-table"), "{out}");
        assert!(!out.contains("harbour.png#rename"), "{out}");
    }

    {
        let mut catalogue = workspace();
        for i in 0..MOST_LISTED + 5 {
            catalogue.offers.push(offer(
                "files",
                &format!("files.entry:/home/q/f{i}.md"),
                "files.entry",
                "rename",
                "Rename",
                InputSpec::None,
            ));
        }
        let mut revealed = Revealed::new();
        let out = operations(&json!({}), &catalogue, &mut revealed);
        assert!(out.contains("more not listed"), "{out}");
        assert!(out.contains("narrower"), "{out}");
        let narrowed = operations(
            &json!({"resource": format!("files.entry:/home/q/f{}.md", MOST_LISTED + 4)}),
            &catalogue,
            &mut revealed,
        );
        assert!(
            narrowed.contains(&format!("f{}.md#rename", MOST_LISTED + 4)),
            "{narrowed}"
        );
    }

    {
        let out = describe(
            &json!({"operation": "files.entry:/home/q/harbour.png#rename"}),
            &workspace(),
            &mut Revealed::new(),
        );
        assert!(out.contains("value: text (required)"), "{out}");
        assert!(out.contains("512 x 320 pixels"), "{out}");
    }

    {
        let out = describe(
            &json!({"operation": "word-processor.document#place-image"}),
            &workspace(),
            &mut Revealed::new(),
        );
        assert!(out.contains("content, not words"), "{out}");
        assert!(out.contains("bound from the prior step"), "{out}");
    }

    {
        let hidden_id = "word-processor.document#imaginary-crop";
        let absent_id = "nothing.at#all";
        let hidden = describe(
            &json!({"operation": hidden_id}),
            &workspace(),
            &mut Revealed::new(),
        );
        let absent = describe(
            &json!({"operation": absent_id}),
            &workspace(),
            &mut Revealed::new(),
        );

        assert_eq!(hidden, nothing_found(&format!("the operation {hidden_id}")));
        assert_eq!(absent, nothing_found(&format!("the operation {absent_id}")));
        assert_eq!(
            hidden.replace(hidden_id, "<id>"),
            absent.replace(absent_id, "<id>"),
            "the two must be indistinguishable once each stops quoting the \
             caller's own words back at it"
        );
        for reply in [&hidden, &absent] {
            let lower = reply.to_lowercase();
            for leak in [
                "permission",
                "denied",
                "allowed",
                "not for you",
                "capabilit",
            ] {
                assert!(!lower.contains(leak), "{reply} leaks {leak:?}");
            }
        }
    }
}

#[test]
fn a_plan_is_read_only_from_revealed_operations_within_the_step_cap() {
    {
        let mut revealed = Revealed::new();
        assert!(!revealed.contains("files.entry:/home/q/harbour.png#rename"));
        // The editorial changes and "none" are the agent's own and are there
        // from the start; nothing an application exports is.
        assert!(revealed.contains(plan::NOTHING_FITS));

        let call = json!({
            "steps": [{"operation": "files.entry:/home/q/harbour.png#rename", "arguments": {"value": "a.png"}}],
            "why": "", "then": null
        });
        let refused = read_plan(&call, &revealed, STEPS).expect_err("nothing revealed it yet");
        assert!(
            refused.contains("no listing in this conversation returned"),
            "{refused}"
        );

        let listed = operations(&json!({"kind": "files.entry"}), &workspace(), &mut revealed);
        assert!(
            listed.contains("files.entry:/home/q/harbour.png#rename"),
            "{listed}"
        );
        let answer = read_plan(&call, &revealed, STEPS).expect("now it has been shown");
        assert_eq!(
            answer.arguments,
            vec![serde_json::Map::from_iter([(
                "value".into(),
                Value::String("a.png".into())
            )])]
        );
    }

    {
        let mut revealed = Revealed::new();
        let _ = operations(&json!({"kind": "files.entry"}), &workspace(), &mut revealed);
        let operation = "files.entry:/home/q/harbour.png#rename";
        let call = json!({
            "steps": [
                {"operation": operation, "arguments": {"value": "first.png"}},
                {"operation": operation, "arguments": {"value": "second.png"}}
            ],
            "why": "the same canonical operation applies to two requested values",
            "then": null
        });
        let answer = read_plan(&call, &revealed, STEPS).expect("both uses were listed");
        assert_eq!(answer.actions, vec![operation, operation]);
        assert_ne!(answer.arguments.first(), answer.arguments.last());
    }

    {
        let mut revealed = Revealed::new();
        revealed.reveal("a#b");
        let steps: Vec<Value> = (0..=STEPS)
            .map(|_| json!({"operation": "a#b", "arguments": {}}))
            .collect();
        let err = read_plan(
            &json!({"steps": steps, "why": "", "then": null}),
            &revealed,
            STEPS,
        )
        .expect_err("over the cap");
        assert!(err.contains("more than the"), "{err}");
    }

    {
        let mut revealed = Revealed::new();
        revealed.reveal("chat.app#open");
        let answer = read_plan(
            &json!({
                "steps": [{"operation": "chat.app#open", "arguments": {}}],
                "why": "the conversation is not open",
                "then": "post it there"
            }),
            &revealed,
            STEPS,
        )
        .expect("read");
        assert_eq!(answer.then.as_deref(), Some("post it there"));
        // A blank is nothing, not a clause whose words are no words.
        let blank = read_plan(
            &json!({
                "steps": [{"operation": "chat.app#open", "arguments": {}}],
                "why": "", "then": "   "
            }),
            &revealed,
            STEPS,
        )
        .expect("read");
        assert_eq!(blank.then, None);
    }

    {
        let mut revealed = Revealed::new();
        // Revealed by one call, so the check below is genuinely about the
        // *other* address rather than an empty `Revealed` refusing anything.
        revealed.reveal("word-processor.document#insert-table");

        let call = json!({
            "steps": [{"operation": "files.entry:/home/q/harbour.png#rename", "arguments": {"value": "a.png"}}],
            "why": "", "then": null
        });
        let err = read_plan(&call, &revealed, STEPS).expect_err("never revealed");
        assert!(
            err.contains("no listing in this conversation returned"),
            "{err}"
        );
        assert!(
            err.contains("files.entry:/home/q/harbour.png#rename"),
            "{err}"
        );
    }
}

/// `reply` is optional, first in the schema, and kept (trimmed, bounded)
/// when the model writes one.
#[test]
fn a_plan_reply_is_optional_first_and_kept() {
    let definition = plan_definition(STEPS);
    let schema = &definition.input_schema;
    let first = schema["properties"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    assert_eq!(first, "reply");
    assert_eq!(schema["properties"]["reply"]["type"], "string");
    assert_eq!(
        schema["properties"]["reply"]["description"],
        PLAN_REPLY_DESCRIPTION
    );
    assert_eq!(schema["required"], json!(["steps", "why", "then"]));

    let mut revealed = Revealed::new();
    revealed.reveal("a#b");
    let step = json!([{"operation": "a#b", "arguments": {}}]);
    let with = read_plan(
        &json!({"reply": "  Sure, writing it now.  ", "steps": step, "why": "", "then": null}),
        &revealed,
        STEPS,
    )
    .expect("read");
    assert_eq!(with.reply.as_deref(), Some("Sure, writing it now."));
    let without = read_plan(
        &json!({"steps": step, "why": "", "then": null}),
        &revealed,
        STEPS,
    )
    .expect("read");
    assert_eq!(without.reply, None);
    let long = "x".repeat(MAX_PLAN_REPLY_CHARS + 50);
    let bounded = read_plan(
        &json!({"reply": long, "steps": step, "why": "", "then": null}),
        &revealed,
        STEPS,
    )
    .expect("read");
    assert_eq!(bounded.reply.unwrap().chars().count(), MAX_PLAN_REPLY_CHARS);
    let wide = "語".repeat(MAX_PLAN_REPLY_CHARS);
    let kept = bound_reply(&wide);
    assert!(kept.len() <= MAX_REPLY_BYTES, "{}", kept.len());
    assert_eq!(kept.chars().count(), MAX_REPLY_BYTES / "語".len());
}

/// A check of finished work may answer that nothing is left, with a
/// reply saying so; anywhere else, and without a reply, no steps is
/// still refused.
#[test]
fn an_empty_plan_is_an_answer_only_where_allowed_and_with_a_reply() {
    let revealed = Revealed::new();
    let none = json!({"reply": " All done. ", "steps": [], "why": "", "then": ""});
    let answer = read_plan_allowing_none(&none, &revealed, STEPS).expect("allowed");
    assert!(answer.actions.is_empty());
    assert_eq!(answer.reply.as_deref(), Some("All done."));
    let refused = read_plan(&none, &revealed, STEPS).expect_err("a first plan");
    assert_eq!(refused, "the plan named nothing to do");
    let silent = json!({"steps": [], "why": "", "then": ""});
    let refused = read_plan_allowing_none(&silent, &revealed, STEPS).expect_err("no reply");
    assert!(refused.contains("say so in `reply`"), "{refused}");
    let blank = json!({"reply": "   ", "steps": [], "why": "", "then": ""});
    assert!(
        read_plan_allowing_none(&blank, &revealed, STEPS).is_err(),
        "a blank reply says nothing"
    );
}

/// The plan tool set names no ids, so it is the same bytes on every turn
/// however much the conversation has revealed; what the first turn's
/// summary showed can be planned with no listing, a prefix-less copy
/// names the one shown operation, and anything else is refused with
/// what to copy instead.
#[test]
fn the_plan_tool_set_is_byte_stable_and_a_plan_names_what_was_shown() {
    let before = serde_json::to_string(&definitions(STEPS)).unwrap();
    let mut revealed = Revealed::new();
    let summary = catalogue_summary(&workspace(), &mut revealed, MOST_SUMMARISED);
    let _ = operations(&json!({}), &workspace(), &mut revealed);
    assert_eq!(
        serde_json::to_string(&definitions(STEPS)).unwrap(),
        before,
        "revealing operations must not change the tool bytes"
    );
    let set = definitions(STEPS);
    let plan = &set[3];
    assert_eq!(plan["name"], PLAN);
    let operation =
        &plan["input_schema"]["properties"]["steps"]["items"]["properties"]["operation"];
    assert_eq!(operation["type"], "string");
    assert!(operation.get("enum").is_none(), "{operation}");
    assert!(
        !before.contains("harbour.png"),
        "no conversation ids in the tools"
    );

    // Shown by the summary alone: planned with no discovery call.
    assert!(
        summary.contains("files.entry:/home/q/harbour.png#rename"),
        "{summary}"
    );
    let mut first_turn = Revealed::new();
    let _ = catalogue_summary(&workspace(), &mut first_turn, MOST_SUMMARISED);
    let answer = read_plan(
        &json!({
            "steps": [{"operation": "files.entry:/home/q/harbour.png#rename", "arguments": {"value": "a.png"}}],
            "why": "", "then": null
        }),
        &first_turn,
        STEPS,
    )
    .expect("the summary showed it");
    assert_eq!(answer.actions, ["files.entry:/home/q/harbour.png#rename"]);
    assert_eq!(
        read_plan(
            &json!({"steps": [{"operation": plan::NOTHING_FITS, "arguments": {}}], "why": "", "then": null}),
            &first_turn,
            STEPS,
        )
        .expect("none is the agent's own")
        .actions,
        [plan::NOTHING_FITS]
    );

    let mut routed = Revealed::new();
    routed.reveal("broker:3:word-processor.document#insert-table");
    let answer = read_plan(
        &json!({"steps": [{"operation": "word-processor.document#insert-table", "arguments": {}}], "why": "", "then": null}),
        &routed,
        STEPS,
    )
    .expect("one shown operation has that address");
    assert_eq!(
        answer.actions,
        ["broker:3:word-processor.document#insert-table"]
    );

    // An unshown id is refused with what to do instead, in words the
    // model can act on.
    let refused = read_plan(
        &json!({"steps": [{"operation": "word-processor.other#insert-table", "arguments": {}}], "why": "", "then": null}),
        &routed,
        STEPS,
    )
    .expect_err("never shown");
    assert!(
        refused.contains("no listing in this conversation returned"),
        "{refused}"
    );
    assert!(refused.contains(LIST_OPERATIONS), "{refused}");
    assert!(
        refused.contains("broker:3:word-processor.document#insert-table"),
        "suggests the shown id for the same action: {refused}"
    );
    let answered = plan_refused(&refused);
    assert!(answered.contains(&refused), "{answered}");
    assert!(answered.contains("nothing was performed"), "{answered}");
}

/// The loop offers only the generic tools; `invoke` can name only what a
/// tool showed, and its operation comes before its arguments so text is
/// attributable while it streams.
#[test]
fn the_loop_tool_set_is_generic_and_invoke_names_only_what_was_shown() {
    let mut revealed = Revealed::new();
    let summary = catalogue_summary(&workspace(), &mut revealed, MOST_SUMMARISED);
    assert!(
        summary.contains("word-processor.document#insert-table"),
        "{summary}"
    );
    assert!(summary.contains("{value: text}"), "{summary}");
    let definitions = loop_definitions();
    assert_eq!(
        definitions,
        loop_definitions(),
        "byte-stable for prompt caching"
    );
    let names: Vec<&str> = definitions
        .as_array()
        .unwrap()
        .iter()
        .map(|definition| definition["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            LIST_APPLICATIONS,
            LIST_OPERATIONS,
            DESCRIBE_OPERATION,
            INVOKE,
            crate::pages::READ_RESULT,
            crate::tools::web::SEARCH_WEB,
            crate::tools::web::READ_PAGE,
            crate::programs::TILO,
            PLAN
        ]
    );
    let invoke = &definitions[3]["input_schema"];
    // Sorted maps on the wire: the operation still comes first.
    let keys: Vec<&String> = invoke["properties"].as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        [
            "operation",
            INVOKE_PARAMETERS,
            PARTS,
            PROGRESS,
            INVOKE_REPLY,
            INVOKE_STANDALONE,
            UNABLE
        ]
    );
    let wire = serde_json::to_string(invoke).unwrap();
    assert!(wire.find("\"operation\"") < wire.find(&format!("\"{INVOKE_PARAMETERS}\"")));
    assert!(invoke["properties"]["operation"].get("enum").is_none());

    let ok = read_invoke(
        &json!({"operation": "files.entry:/home/q/harbour.png#rename", "parameters": {"value": "a.png"}}),
        &revealed,
    )
    .expect("shown in the summary");
    assert_eq!(ok.arguments["value"], "a.png");
    let refused = read_invoke(&json!({"operation": "x#y", "arguments": {}}), &revealed)
        .expect_err("never shown");
    assert!(refused.contains("list_operations"), "{refused}");
    assert!(
        read_invoke(
            &json!({"operation": "files.entry:/home/q/harbour.png#rename", "arguments": 3}),
            &revealed
        )
        .is_err()
    );
    assert_eq!(
        read_finish(&json!({"reply": "  Done.  ", "done": false})),
        Finish {
            reply: Some("Done.".into()),
            done: false,
            view: None,
        }
    );
}

/// An observation is compact JSON and bounded; a long value is not cut
/// but continues on a later page.
#[test]
fn an_invoke_observation_is_compact_and_bounded() {
    let mut pages = crate::pages::Pages::new();
    let long = "x".repeat(crate::pages::PAGE_CHARS * 2);
    let report = InvokeReport {
        succeeded: true,
        value: Some(Value::String(long.clone())),
        changed_view: Some(json!({"caret": {"block": "b1", "offset": 3}})),
        newly_available: vec!["a#b — B".into()],
        ..InvokeReport::default()
    };
    let text = invoke_result(&report, &mut pages);
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["result"], "succeeded");
    assert_eq!(value["changed_view"]["caret"]["offset"], 3);
    assert_eq!(value["newly_available"][0], "a#b — B");
    assert!(
        text.len() < crate::pages::PAGE_CHARS + 600,
        "{}",
        text.len()
    );
    let from = value["continues"]["from"].as_u64().unwrap();
    let rest: Value = serde_json::from_str(
        &pages.read(&json!({"result": value["continues"]["result"], "from": from})),
    )
    .unwrap();
    assert_eq!(rest["next"], Value::Null);
    assert_eq!(
        format!(
            "{}{}",
            value["value"].as_str().unwrap(),
            rest["text"].as_str().unwrap()
        ),
        long
    );
    let skipped = invoke_result(
        &InvokeReport {
            skipped: Some("an earlier invoke failed".into()),
            ..InvokeReport::default()
        },
        &mut pages,
    );
    assert_eq!(
        serde_json::from_str::<Value>(&skipped).unwrap()["result"],
        "not run"
    );
}

/// Every open object in the tool schemas says it is open. Gemini reads an
/// object schema without properties as an empty object, so an argument
/// map left implicit would reach every operation empty and a required
/// argument (a new note's title, say) could never be passed.
#[test]
fn every_open_argument_object_declares_itself_open() {
    fn walk(path: &str, node: &Value, open: &mut Vec<String>) {
        match node {
            Value::Object(map) => {
                if map.get("type").and_then(Value::as_str) == Some("object")
                    && !map.contains_key("properties")
                    && map.get("additionalProperties") != Some(&Value::Bool(true))
                {
                    open.push(path.to_owned());
                }
                for (key, child) in map {
                    walk(&format!("{path}/{key}"), child, open);
                }
            }
            Value::Array(items) => {
                for (index, child) in items.iter().enumerate() {
                    walk(&format!("{path}/{index}"), child, open);
                }
            }
            _ => {}
        }
    }
    let mut undeclared = Vec::new();
    walk("", &definitions(4), &mut undeclared);
    assert!(
        undeclared.is_empty(),
        "implicitly open objects: {undeclared:?}"
    );
}

/// Every tool schema stays inside the JSON Schema subset every provider
/// dialect the agent speaks accepts as function parameters, Gemini's
/// included: plain types, no unions, no `null` type, no references.
#[test]
fn tool_schemas_use_only_keywords_every_provider_accepts() {
    const ACCEPTED: &[&str] = &[
        "type",
        "description",
        "properties",
        "required",
        "items",
        "enum",
        "minimum",
        "maximum",
        "minItems",
        "maxItems",
        "additionalProperties",
    ];
    fn check(schema: &Value, at: &str) {
        let Some(object) = schema.as_object() else {
            panic!("{at}: a schema is an object");
        };
        for (key, value) in object {
            assert!(ACCEPTED.contains(&key.as_str()), "{at}: `{key}`");
            match key.as_str() {
                "type" => assert!(
                    value.as_str().is_some_and(|ty| ty != "null"),
                    "{at}: one non-null type"
                ),
                "properties" => {
                    for (name, property) in value.as_object().unwrap() {
                        check(property, &format!("{at}.{name}"));
                    }
                }
                "items" => check(value, &format!("{at}[]")),
                _ => {}
            }
        }
    }
    for set in [
        definitions(8),
        loop_definitions(),
        json!(typed_waiting_definitions(8, &["mail", "chat"], 4)),
    ] {
        for tool in set.as_array().unwrap() {
            check(&tool["input_schema"], tool["name"].as_str().unwrap());
        }
    }
}

/// An operation taking content beside words lists its other parameters
/// and says the content one is carried in from the step before it.
#[test]
fn content_beside_words_shows_the_words_and_carries_the_content() {
    let mut attach = offer(
        "mail",
        "mail.mailbox",
        "mail.mailbox",
        "attach",
        "Attach to draft",
        InputSpec::None,
    );
    attach.descriptor.params = vec![
        tilo_actions::ActionParam::required("content", tilo_actions::ParamType::Text, "file")
            .as_content(),
        tilo_actions::ActionParam::required("name", tilo_actions::ParamType::Text, "name"),
    ];
    assert_eq!(
        parameter_signature(&attach),
        "{content: what the previous step produced, name: text}"
    );
    let hint = parameter_hint(&attach);
    assert!(hint.contains("\"name\": …"), "{hint}");
    assert!(!hint.contains("\"content\": …"), "{hint}");
    assert!(hint.contains("`content` is filled in"), "{hint}");
    let place = &workspace().offers[1];
    assert_eq!(
        parameter_signature(place),
        "{takes what the previous step produced}"
    );
}
