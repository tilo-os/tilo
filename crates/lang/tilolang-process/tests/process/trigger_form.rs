//! A person fills the trigger form of a Shortcut's draft: the form's fields are the draft's
//! own record, a record the workbench keeps inside the document its window shows, so what the
//! window sends names the document while the text belongs to the record.

use super::*;

const SHORTCUTS: &str = "so.tilo.shortcuts";

/// The shipped Shortcuts package, built with the packages it uses.
fn shortcuts() -> Bundle {
    shipped(&shipped_dir().join("shortcuts")).1
}

/// The document the window shows, as the app publishes it.
fn shown_document(host: &TilolangProcess) -> patch::ResourceRef {
    let instance = host
        .instances()
        .next()
        .expect("the instance runs")
        .instance();
    let document = instance
        .host()
        .expect("the instance has a host")
        .root_of(SHORTCUTS)
        .expect("the workbench is the app's root");
    tilolang_tilo::instance::wire::resource(&document)
}

/// The node of the window's root tree of `kind` whose prop `name` reads `value`.
fn keyed(window: &Window, kind: &str, name: &str, value: &str) -> String {
    let root = window.root.as_ref().expect("the window shows its tree");
    root.of_kind(kind)
        .into_iter()
        .find(|node| {
            node.props
                .get(name)
                .is_some_and(|prop| format!("{prop:?}").contains(value))
        })
        .unwrap_or_else(|| panic!("a {kind} whose {name} reads {value:?}: {}", shown(window)))
        .key
        .clone()
}

/// Press the control `key`, firing `event`.
fn press(window: &mut Window, key: &str, event: &str) {
    window.send(&[ToApp::Fire {
        level: Level::Root,
        key: key.to_owned(),
        event: event.to_owned(),
        value: None,
        gesture: None,
    }]);
}

/// A person starts a draft, sets what starts it and the app id there, and adds the trigger:
/// each key typed into the field is spliced into the text the field holds for the record it
/// edits, and the trigger the draft takes holds it.
#[test]
fn typing_an_app_id_into_the_trigger_form_reaches_the_draft() {
    let (mut host, mut window, _sent, _inbox, _route) = running_app("ada", SHORTCUTS, shortcuts());
    settle(&mut host, &mut window);

    // New shortcut opens a draft, its trigger form drawn in the window.
    let (new, event) = button_labelled(&window, "New shortcut");
    press(&mut window, &new, &event);
    settle(&mut host, &mut window);
    assert!(
        shown(&window).contains("Add trigger"),
        "the draft's trigger form stands: {}",
        shown(&window)
    );
    let document = shown_document(&host);

    // What starts it: the case field takes "App opened", as the window sends a picked case.
    let kind = keyed(&window, "ui.EnumField", "label", "What starts it");
    window.send(&[ToApp::Edit {
        level: Level::Root,
        target: document.clone(),
        key: kind,
        text: "app_opened".to_owned(),
    }]);
    settle(&mut host, &mut window);

    // The form asks for the app now. The person types it, one key at a time, as the keyboard
    // sends each: a splice of the text the field holds for the record it edits.
    let input = keyed(&window, "ui.TextInput", "placeholder", "so.tilo.notes");
    let typed = "so.tilo.chat";
    for (seq, key) in typed.chars().enumerate() {
        let at = u32::try_from(seq).expect("a short app id");
        let shown_so_far: String = typed.chars().take(seq.saturating_add(1)).collect();
        window.send(&[key_edit(
            false,
            Level::Root,
            &document,
            &input,
            seq,
            at,
            &shown_so_far,
            key,
        )]);
        settle(&mut host, &mut window);
        assert_eq!(
            window.bound.get(&input),
            Some(&patch::Value::Text(shown_so_far.clone())),
            "the app holds the text typed so far (`{shown_so_far}`): {}",
            shown(&window)
        );
    }

    // Add trigger takes the form into the draft: the trigger stands in it with the app it is for.
    let (add, event) = button_labelled(&window, "Add trigger");
    press(&mut window, &add, &event);
    settle(&mut host, &mut window);
    let root = window.root.as_ref().expect("the window shows its tree");
    let trigger = root
        .of_kind("ui.ListRow")
        .into_iter()
        .find(|row| {
            row.props
                .get("title")
                .is_some_and(|title| format!("{title:?}").contains("An app opens"))
        })
        .unwrap_or_else(|| panic!("the draft lists its trigger: {}", shown(&window)));
    assert_eq!(
        trigger.props.get("secondary"),
        Some(&patch::Value::Text(typed.to_owned())),
        "the trigger is for the app the person typed"
    );
}
