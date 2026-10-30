//! The portal's questions on the session's sheets: what an app asks of the
//! person, and how each kind is answered through the sheets' actions.

use super::*;

use futures_util::FutureExt as _;
use tilo_service::{Answer, Asked};

/// A session whose services ask their questions on its sheets.
fn asking() -> (Session, tilo_service::Questions) {
    let questions = tilo_service::Questions::new();
    let session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            questions: Some(questions.clone()),
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    (session, questions)
}

/// Ask `kind` on its sheet, answered by the app it is asked for.
fn ask(questions: &tilo_service::Questions, kind: &str, details: serde_json::Value) -> Asked {
    questions.ask(tilo_service::Question {
        handle: format!("portal:/request/{kind}"),
        kind: kind.to_owned(),
        app_id: Some("org.example.Recorder".to_owned()),
        label: "org.example.Recorder".to_owned(),
        details,
    })
}

/// The answer to `asked`, once the sheets answered it.
#[track_caller]
fn answered(session: &mut Session, questions: &tilo_service::Questions, asked: Asked) -> Answer {
    pump(session, &mut [], |_, _| questions.pending().is_empty());
    asked
        .now_or_never()
        .expect("the question is answered")
        .expect("answered, not declined")
}

/// Press `effect` on the sheet publishing it, as the person would: a
/// portal answer names the question that is up.
fn press(session: &mut Session, effect: &str, args: serde_json::Value) {
    let mut args = arguments(args);
    let handle = session
        .portal()
        .and_then(|portal| portal.dialog())
        .map(|dialog| dialog.handle);
    if let Some(handle) = handle
        && !args.contains_key("handle")
    {
        args.insert("handle".to_owned(), handle.into());
    }
    let sheet = publisher(session, effect).expect("the sheet publishes that");
    session.invoke(&sheet, effect, &args).expect("press");
    session.step(Duration::from_millis(1)).expect("step");
}

#[test]
fn the_access_question_is_answered_on_the_portal_sheet() {
    use tilo_portal::consent::AccessQuestion;
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = asking();
    let asked = ask(
        &questions,
        "portal.access",
        serde_json::to_value(AccessQuestion {
            capability: "screencast".to_owned(),
            reason: None,
            may_remember: true,
            text: None,
        })
        .unwrap(),
    );
    session.step(Duration::from_millis(1)).expect("step");
    let dialog = session
        .portal()
        .expect("portal sheet")
        .dialog()
        .expect("the sheet asks");
    assert_eq!(
        dialog.title,
        "org.example.Recorder wants to use your screen"
    );
    assert_eq!(
        dialog.body,
        Body::Choices(vec![
            "Allow once".to_owned(),
            "Allow for this session".to_owned(),
            "Always allow".to_owned(),
        ])
    );
    press(
        &mut session,
        PortalEffect::SELECT.id,
        serde_json::json!({"index": 2}),
    );
    press(
        &mut session,
        PortalEffect::CONFIRM.id,
        serde_json::json!({}),
    );
    let answer = answered(&mut session, &questions, asked);
    assert_eq!(answer.choice, Some(2), "allowed for good");
    assert!(
        session.portal().expect("portal sheet").dialog().is_none(),
        "the sheet is done"
    );
}

#[test]
fn the_question_of_an_app_that_may_not_remember_offers_no_always() {
    use tilo_portal::consent::AccessQuestion;
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = asking();
    let asked = ask(
        &questions,
        "portal.access",
        serde_json::to_value(AccessQuestion {
            capability: "camera".to_owned(),
            reason: Some("Look at the barcode".to_owned()),
            may_remember: false,
            text: None,
        })
        .unwrap(),
    );
    session.step(Duration::from_millis(1)).expect("step");
    let dialog = session.portal().expect("portal sheet").dialog().unwrap();
    assert_eq!(dialog.detail, "Look at the barcode");
    assert_eq!(
        dialog.body,
        Body::Choices(vec![
            "Allow once".to_owned(),
            "Allow for this session".to_owned(),
        ])
    );
    press(&mut session, PortalEffect::CANCEL.id, serde_json::json!({}));
    pump(&mut session, &mut [], |_, _| questions.pending().is_empty());
    assert!(
        asked.now_or_never().expect("answered").is_none(),
        "declined"
    );
}

#[test]
fn the_chooser_and_the_launcher_name_the_app_in_their_words() {
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = asking();
    let asked = ask(
        &questions,
        tilo_portal::app_chooser::APP_CHOOSER_QUESTION,
        serde_json::json!({
            "choices": [{"id": "org.example.Notes", "name": "Notes", "icon": null}],
            "last_choice": null,
            "content_type": "text/markdown",
            "uri": null,
            "filename": "notes.md",
        }),
    );
    session.step(Duration::from_millis(1)).expect("step");
    let dialog = session
        .portal()
        .expect("portal sheet")
        .dialog()
        .expect("the sheet asks");
    assert_eq!(dialog.detail, "org.example.Recorder wants to open notes.md");
    press(&mut session, PortalEffect::CANCEL.id, serde_json::json!({}));
    pump(&mut session, &mut [], |_, _| questions.pending().is_empty());
    assert!(
        asked.now_or_never().expect("answered").is_none(),
        "declined"
    );

    let asked = ask(
        &questions,
        tilo_portal::dynamic_launcher::LAUNCHER_QUESTION,
        serde_json::json!({
            "name": "Inbox",
            "editable_name": true,
            "editable_icon": false,
            "icon": "",
            "target": "https://example.org",
        }),
    );
    session.step(Duration::from_millis(1)).expect("step");
    let dialog = session
        .portal()
        .expect("portal sheet")
        .dialog()
        .expect("the sheet asks");
    assert_eq!(
        dialog.detail,
        "org.example.Recorder adds a launcher. The image it supplied has been checked."
    );
    press(&mut session, PortalEffect::CANCEL.id, serde_json::json!({}));
    pump(&mut session, &mut [], |_, _| questions.pending().is_empty());
    assert!(
        asked.now_or_never().expect("answered").is_none(),
        "declined"
    );
}

#[test]
fn every_portal_question_kind_is_asked_on_the_sheet() {
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = asking();
    let source = serde_json::json!({
        "choice": "output:HEADLESS-1",
        "kind": "output",
        "label": "HEADLESS-1",
        "app_id": null,
    });
    let asked_kinds = [
        (
            "portal.account",
            serde_json::json!({"reason": "Check your calendar"}),
        ),
        (
            "portal.email",
            serde_json::json!({
                "addresses": ["ada@example.org"],
                "cc": [],
                "bcc": [],
                "subject": "Holiday",
                "attachments": [],
            }),
        ),
        (
            "portal.background",
            serde_json::json!({"name": "Backing up photos"}),
        ),
        (
            "portal.wallpaper",
            serde_json::json!({
                "uri": "file:///var/home/you/beach.png",
                "set_on": "both",
                "preview": false,
            }),
        ),
        (
            "portal.print",
            serde_json::json!({
                "title": "notes.odt",
                "printers": ["Office"],
                "selected": 0,
                "copies": 1,
            }),
        ),
        (
            "portal.screenshot",
            serde_json::json!({"parent_window": null, "modal": true}),
        ),
        (
            "portal.launcher",
            serde_json::json!({
                "name": "Inbox",
                "editable_name": true,
                "editable_icon": true,
                "icon": "",
                "target": "https://example.org",
            }),
        ),
        (
            "portal.screencast",
            serde_json::json!({
                "sources": [source],
                "multiple": false,
                "may_remember": true,
                "devices": [],
            }),
        ),
        (
            "portal.remote-desktop",
            serde_json::json!({
                "sources": [source],
                "multiple": false,
                "may_remember": true,
                "devices": ["keyboard"],
            }),
        ),
    ];
    for (kind, details) in asked_kinds {
        let asked = ask(&questions, kind, details);
        session.step(Duration::from_millis(1)).expect("step");
        assert!(
            session.portal().expect("portal sheet").dialog().is_some(),
            "{kind} is asked on the sheet"
        );
        press(&mut session, PortalEffect::CANCEL.id, serde_json::json!({}));
        pump(&mut session, &mut [], |_, _| questions.pending().is_empty());
        assert!(
            asked.now_or_never().expect("answered").is_none(),
            "{kind} declined"
        );
    }
}

#[test]
fn the_colour_picker_answers_what_was_typed_into_the_sheet() {
    use tilo_portal::screenshot::ScreenshotQuestion;
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = asking();
    let asked = ask(
        &questions,
        "portal.pick-color",
        serde_json::to_value(ScreenshotQuestion {
            parent_window: None,
            modal: true,
        })
        .unwrap(),
    );
    session.step(Duration::from_millis(1)).expect("step");
    press(
        &mut session,
        PortalEffect::CONFIRM.id,
        serde_json::json!({}),
    );
    let error = session
        .portal()
        .expect("portal sheet")
        .dialog()
        .and_then(|dialog| dialog.error);
    assert!(error.is_some(), "nothing typed is no colour");
    assert_eq!(questions.pending().len(), 1, "the question waits");
    for (at, value) in ["0.5", " 0.25 ", "1"].iter().enumerate() {
        press(
            &mut session,
            PortalEffect::TYPE.id,
            serde_json::json!({"field": format!("draft-{at}"), "text": value}),
        );
    }
    press(
        &mut session,
        PortalEffect::CONFIRM.id,
        serde_json::json!({}),
    );
    let answer = answered(&mut session, &questions, asked);
    let values: Vec<&str> = answer.values.iter().map(|value| value.as_str()).collect();
    assert_eq!(values, ["0.5", " 0.25 ", "1"]);
}

#[test]
fn the_usb_question_answers_the_ticked_devices() {
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = asking();
    let asked = ask(
        &questions,
        "portal.usb",
        serde_json::json!({
            "devices": [
                { "id": "usb-1", "label": "Reader", "writable": false },
                { "id": "usb-2", "label": null, "writable": true },
            ],
        }),
    );
    session.step(Duration::from_millis(1)).expect("step");
    let dialog = session.portal().expect("portal sheet").dialog().unwrap();
    assert_eq!(
        dialog.body,
        Body::Devices(vec![
            tilo_sheets::portal::Device {
                label: "Reader".to_owned(),
                writable: false,
                selected: true,
            },
            tilo_sheets::portal::Device {
                label: "usb-2".to_owned(),
                writable: true,
                selected: true,
            },
        ])
    );
    press(
        &mut session,
        PortalEffect::TOGGLE.id,
        serde_json::json!({"index": 1}),
    );
    press(
        &mut session,
        PortalEffect::CONFIRM.id,
        serde_json::json!({}),
    );
    let answer = answered(&mut session, &questions, asked);
    assert_eq!(answer.uris, ["usb-1"], "only what stayed ticked");
}

#[test]
fn the_app_chooser_answers_the_app_chosen_and_starts_on_the_last_one() {
    use tilo_portal::app_chooser::{AppChoice, AppChooserQuestion};
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = asking();
    let asked = ask(
        &questions,
        "portal.app-chooser",
        serde_json::to_value(AppChooserQuestion {
            choices: vec![
                AppChoice {
                    id: "org.example.Viewer".to_owned(),
                    name: "Viewer".to_owned(),
                    icon: None,
                },
                AppChoice {
                    id: "org.example.Editor".to_owned(),
                    name: "Editor".to_owned(),
                    icon: None,
                },
            ],
            last_choice: Some("org.example.Editor".to_owned()),
            content_type: Some("image/png".to_owned()),
            uri: None,
            filename: Some("beach.png".to_owned()),
        })
        .unwrap(),
    );
    session.step(Duration::from_millis(1)).expect("step");
    let dialog = session.portal().expect("portal sheet").dialog().unwrap();
    assert_eq!(dialog.title, "Open with what?");
    assert_eq!(dialog.selected, 1, "the app chosen last time");
    press(
        &mut session,
        PortalEffect::SELECT.id,
        serde_json::json!({"index": 0}),
    );
    press(
        &mut session,
        PortalEffect::CONFIRM.id,
        serde_json::json!({}),
    );
    let answer = answered(&mut session, &questions, asked);
    assert_eq!(answer.choice, Some(0));
}

#[test]
fn the_shortcut_question_answers_the_keys_pressed_in_the_sheet() {
    use tilo_portal::global_shortcuts::{BIND_AS_PROPOSED, ShortcutRow, ShortcutsQuestion};
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = asking();
    let shortcuts = ShortcutsQuestion {
        shortcuts: vec![ShortcutRow {
            id: "record".to_owned(),
            description: Some("Start recording".to_owned()),
            trigger: Some("Ctrl+Shift+K".to_owned()),
            status: None,
            taken_by: None,
        }],
    };
    let asked = ask(
        &questions,
        "portal.global-shortcuts",
        serde_json::to_value(&shortcuts).unwrap(),
    );
    session.step(Duration::from_millis(1)).expect("step");
    let dialog = session.portal().expect("portal sheet").dialog().unwrap();
    assert_eq!(
        dialog.body,
        Body::Shortcuts(vec![tilo_sheets::portal::Shortcut {
            name: "Start recording".to_owned(),
            keys: Some("Ctrl + Shift + K".to_owned()),
            status: None,
        }])
    );
    press(
        &mut session,
        PortalEffect::CONFIRM.id,
        serde_json::json!({}),
    );
    let answer = answered(&mut session, &questions, asked);
    assert_eq!(answer.choice, Some(BIND_AS_PROPOSED));

    let (mut session, questions) = asking();
    let asked = ask(
        &questions,
        "portal.global-shortcuts",
        serde_json::to_value(&shortcuts).unwrap(),
    );
    session.step(Duration::from_millis(1)).expect("step");
    press(
        &mut session,
        PortalEffect::BIND.id,
        serde_json::json!({"keys": "ALT+KEYJ"}),
    );
    let keys = session
        .portal()
        .expect("portal sheet")
        .dialog()
        .map(|dialog| dialog.body);
    assert_eq!(
        keys,
        Some(Body::Shortcuts(vec![tilo_sheets::portal::Shortcut {
            name: "Start recording".to_owned(),
            keys: Some("Alt + J".to_owned()),
            status: None,
        }]))
    );
    press(
        &mut session,
        PortalEffect::CONFIRM.id,
        serde_json::json!({}),
    );
    let answer = answered(&mut session, &questions, asked);
    assert_eq!(answer.uris, ["record=ALT+KEYJ"]);
}

#[test]
fn a_key_without_a_modifier_is_not_taken_as_a_shortcut() {
    use tilo_portal::global_shortcuts::{ShortcutRow, ShortcutsQuestion};
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = asking();
    let _asked = ask(
        &questions,
        "portal.global-shortcuts",
        serde_json::to_value(ShortcutsQuestion {
            shortcuts: vec![ShortcutRow {
                id: "record".to_owned(),
                description: None,
                trigger: None,
                status: None,
                taken_by: None,
            }],
        })
        .unwrap(),
    );
    session.step(Duration::from_millis(1)).expect("step");
    press(
        &mut session,
        PortalEffect::BIND.id,
        serde_json::json!({"keys": "KEYJ"}),
    );
    let body = session
        .portal()
        .expect("portal sheet")
        .dialog()
        .map(|dialog| dialog.body);
    let Some(Body::Shortcuts(rows)) = body else {
        panic!("the sheet shows its rows");
    };
    assert_eq!(rows.first().and_then(|row| row.keys.clone()), None);
    assert!(rows.first().and_then(|row| row.status.clone()).is_some());
}

#[test]
fn the_file_chooser_question_is_answered_with_the_file_chosen() {
    let _settled = ui::motion::testing::settled();
    // The portal's chooser starts in the person's home and may go no
    // further than it, so the picture is made inside.
    let home = tempfile::Builder::new()
        .prefix("tilo-chooser-")
        .tempdir_in(tilo_paths::home().expect("a home to browse"))
        .unwrap();
    std::fs::write(home.path().join("beach.png"), b"bytes").unwrap();
    let (mut session, questions) = asking();
    let asked = ask(
        &questions,
        "portal.file-chooser",
        serde_json::json!({
            "parent_window": null,
            "title": "Choose a picture",
            "save": false,
            "directory": false,
            "writable_directory": false,
            "multiple": false,
            "accept_label": null,
            "current_name": null,
            "filters": [],
        }),
    );
    session.step(Duration::from_millis(1)).expect("step");
    let seat = session.file_picker().expect("file chooser").clone();
    let beach = home.path().join("beach.png");
    // The chooser reads the folder off the loop: walk into the folder once
    // its start is listed, and choose once the folder is.
    pump(&mut session, &mut [], |_, _| {
        seat.request().is_some_and(|request| {
            request.handle.ends_with("portal.file-chooser") && request.title == "Choose a picture"
        }) && !seat.listing().reading
    });
    let handle = || seat.request().expect("a request").handle;
    let navigate = FilePickerEffect::NAVIGATE.id;
    let sheet = publisher(&session, navigate).expect("the chooser publishes walking");
    session
        .invoke(
            &sheet,
            navigate,
            &arguments(
                serde_json::json!({"handle": handle(), "path": home.path().to_string_lossy()}),
            ),
        )
        .expect("go");
    pump(&mut session, &mut [], |_, _| {
        let listing = seat.listing();
        !listing.reading && listing.entries.iter().any(|entry| entry.path == beach)
    });
    // Choosing takes the choice at once; confirming checks it off the loop.
    let select = FilePickerEffect::SELECT.id;
    let sheet = publisher(&session, select).expect("the chooser publishes choosing");
    session
        .invoke(
            &sheet,
            select,
            &arguments(serde_json::json!({"handle": handle(), "path": beach.to_string_lossy()})),
        )
        .expect("select");
    pump(&mut session, &mut [], |_, _| seat.listing().can_confirm);
    session
        .invoke(
            &sheet,
            FilePickerEffect::CONFIRM.id,
            &arguments(serde_json::json!({"handle": handle()})),
        )
        .expect("confirm");
    let answer = answered(&mut session, &questions, asked);
    assert_eq!(answer.uris.len(), 1);
    assert!(
        answer.uris[0].ends_with("/beach.png"),
        "the file chosen: {}",
        answer.uris[0]
    );
}

#[test]
fn the_password_of_an_administrator_question_is_answered_as_it_was_typed() {
    use tilo_polkit_agent::questions::AuthenticateDetails;
    let _settled = ui::motion::testing::settled();
    let (mut session, questions) = asking();
    let asked = questions.ask(tilo_service::Question {
        handle: "polkit:cookie-1".to_owned(),
        kind: "polkit.authenticate".to_owned(),
        app_id: None,
        label: "Authentication is required to install software".to_owned(),
        details: serde_json::to_value(AuthenticateDetails {
            message: "Authentication is required to install software".to_owned(),
            action_id: "org.freedesktop.packagekit.package-install".to_owned(),
            identities: vec!["person".to_owned()],
            identity: 0,
            prompt: "Password:".to_owned(),
            echo: false,
            notice: None,
        })
        .unwrap(),
    });
    session.step(Duration::from_millis(1)).expect("step");
    let dialog = session.portal().expect("portal sheet").dialog().unwrap();
    assert_eq!(
        dialog.title,
        "Authentication is required to install software"
    );
    assert!(matches!(dialog.body, Body::Secret(_)), "a password prompt");
    press(
        &mut session,
        PortalEffect::TYPE_SECRET.id,
        serde_json::json!({"field": 0, "value": "hunter2"}),
    );
    press(&mut session, PortalEffect::SUBMIT.id, serde_json::json!({}));
    let answer = answered(&mut session, &questions, asked);
    let values: Vec<&str> = answer.values.iter().map(|value| value.as_str()).collect();
    assert_eq!(values, ["hunter2"]);
}
