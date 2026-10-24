//! What follows an extension's keyword in the address reaches it, and a
//! script or an agent sends it words as Enter does.

use super::*;

#[test]
fn what_follows_an_extensions_keyword_reaches_it_and_enter_sends_it_there() {
    use crate::extension_omnibox::OmniboxRow;
    use ui::webview::{OmniboxInput, OmniboxSuggestion};
    let window = ui::webview::test_engine::TestEngine::new();
    let shared: SharedEngine = window.clone();
    let mut controller = ExtensionController::new(true);
    let (finder, off) = ("f".repeat(32), "e".repeat(32));
    // Their records, which requests find them by beside the toolbar's.
    controller.infos = [&finder, &off]
        .into_iter()
        .map(|id| ExtensionInfo {
            id: id.clone(),
            name: id.clone(),
            version: "1".into(),
            manifest_version: 3,
            description: String::new(),
            permissions: Vec::new(),
            path: std::path::PathBuf::new(),
        })
        .collect();
    controller.installed = vec![
        Installed {
            keyword: Some("Kw".into()),
            ..installed(&finder, None, toolbar::Access::default())
        },
        Installed {
            keyword: Some("off".into()),
            enabled: false,
            ..installed(&off, None, toolbar::Access::default())
        },
    ];
    let told = || {
        window
            .commands()
            .into_iter()
            .filter_map(|command| match command {
                WebViewCommand::ExtensionOmnibox { id, input } => Some((id, input)),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    // The keyword alone, and a turned-off extension's, are the omnibox's.
    assert_eq!(controller.omnibox_typed("kw", &shared), None);
    assert_eq!(controller.omnibox_typed("off cats", &shared), None);
    assert!(told().is_empty());
    // The keyword with a space, whatever its case: the extension's.
    assert_eq!(controller.omnibox_typed("kw ca", &shared), Some(true));
    assert_eq!(
        told(),
        [
            (finder.clone(), OmniboxInput::Started),
            (
                finder.clone(),
                OmniboxInput::Changed {
                    text: "ca".into(),
                    request: 1
                }
            ),
        ]
    );
    assert!(controller.on_engine_event(
        WebViewEvent::ExtensionOmniboxSuggestions {
            id: finder.clone(),
            request: 1,
            suggestions: vec![OmniboxSuggestion {
                content: "cat".into(),
                description: "<match>ca</match>t".into(),
            }],
        },
        &shared
    ));
    let (keyword, rows) = controller.omnibox_rows().unwrap();
    assert_eq!(keyword.keyword, "Kw");
    assert_eq!(
        rows,
        [
            OmniboxRow {
                title: "ca".into(),
                content: "ca".into()
            },
            OmniboxRow {
                title: "cat".into(),
                content: "cat".into()
            },
        ]
    );

    // Enter: the extension is sent the words, and its rows are gone.
    let request = controller.omnibox_enter("kw cat").unwrap();
    assert_eq!(
        request,
        ExtensionRequest::Send(finder.clone(), "cat".into())
    );
    controller.request(&request).unwrap();
    let commands = std::mem::take(&mut *controller.commands.lock().unwrap());
    for command in commands {
        controller.run(command, &shared);
    }
    assert_eq!(
        told().last(),
        Some(&(finder.clone(), OmniboxInput::Entered { text: "cat".into() }))
    );
    assert!(controller.omnibox_rows().is_none());
    assert_eq!(controller.omnibox_enter("cats"), None);
    // One turned off takes nothing, nor more than the engine carries.
    assert!(
        controller
            .request(&ExtensionRequest::Send(off, "x".into()))
            .is_err()
    );
    assert!(
        controller
            .request(&ExtensionRequest::Send(finder, "x".repeat(2049)))
            .is_err()
    );
    // A private window has none.
    assert_eq!(
        ExtensionController::new(false).omnibox_typed("kw ca", &shared),
        None
    );
}

#[test]
fn a_script_or_an_agent_sends_an_extension_words_as_its_keyword_does() {
    use ui::tilo_resource::Effect as _;
    let profile = tempfile::tempdir().unwrap();
    let path = profile.path().join("finder");
    std::fs::create_dir(&path).unwrap();
    std::fs::write(
        path.join("manifest.json"),
        r#"{"manifest_version":3,"name":"Finder","version":"1","omnibox":{"keyword":"fd"}}"#,
    )
    .unwrap();
    let info = ExtensionInfo {
        id: "finder".into(),
        name: "Finder".into(),
        version: "1".into(),
        manifest_version: 3,
        description: String::new(),
        permissions: Vec::new(),
        path,
    };
    let on = Installed::read(&info, true, false).unwrap();
    assert_eq!(on.keyword.as_deref(), Some("fd"));
    let id = on.id.clone();
    let mut controller = ExtensionController::new(true);
    controller.infos = vec![info];
    controller.installed = vec![on.clone()];
    let resource = controller.resources().remove(0);
    let spec = resource
        .actions
        .iter()
        .find(|spec| spec.id == "send-to-extension")
        .expect("an extension with a keyword takes words");
    let mut arguments = serde_json::Map::new();
    arguments.insert("text".into(), "rust".into());
    let encoded = spec.build_json(&arguments).unwrap();
    let operation = crate::effects::BrowserEffect::from_payload(&encoded.payload)
        .unwrap()
        .operation();
    let crate::effects::BrowserOperation::Extension(request) = operation else {
        panic!("{operation:?} is no extension request");
    };
    assert_eq!(request, ExtensionRequest::Send(id.clone(), "rust".into()));
    controller.request(&request).unwrap();
    assert_eq!(queued(&controller), [format!("Send({id:?}, \"rust\")")]);
    // Off, it offers none.
    controller.installed = vec![Installed {
        enabled: false,
        ..on
    }];
    assert!(
        controller.resources()[0]
            .actions
            .iter()
            .all(|spec| spec.id != "send-to-extension")
    );
}
