use super::*;
use crate::webview::{FileChooser, FileChooserMode};

#[test]
fn a_persons_file_chooser_reaches_only_a_tab_that_answers_them() {
    let tx = Arc::new(CommandQueue::default());
    let shared = Arc::new(Shared::default());
    let routes = Arc::new(Mutex::new(HashMap::from([(1, Arc::downgrade(&shared))])));
    let runtime = Arc::new(Runtime {
        tx: tx.clone(),
        process: shared.clone(),
        supervisor: Mutex::default(),
        routes,
        next_tab: AtomicU32::new(2),
        download_events: Arc::default(),
        server: Arc::default(),
        link: None,
        service_session: None,
        exited: Arc::new(AtomicBool::new(false)),
    });
    let engine = ChromiumEngine {
        tx: tx.clone(),
        shared,
        runtime: runtime.clone(),
        tab: 1,
    };
    engine.accept_file_choosers();
    assert!(engine.drain_events().is_empty());
    let queued = || tx.state.lock().unwrap().0.len();
    assert_eq!(queued(), 0, "an older host never sees the command");

    let (ready, _receiver) = sync_channel(1);
    let route = |event: Value| {
        route_native_event(&event, &runtime.routes, &runtime.download_events, &ready)
    };
    route(
        json!({"event":"ready","cef_version":"152","chromium_version":"152","file_choosers":true}),
    )
    .unwrap();
    engine.drain_events();
    if file_chooser::EMBEDDER_ANSWERS {
        let asked = tx.pop().expect("told once the host holds choosers");
        assert_eq!(asked, json!({"op":"file_choosers","tab":1,"enabled":true}));
    }
    engine.drain_events();
    assert_eq!(
        queued(),
        0,
        "told once, and never where the host keeps its platform's panel"
    );

    route(
        json!({"event":"file_chooser","tab":1,"id":7,"mode":"open_multiple","title":"",
        "accept":[".pdf","image/*"]}),
    )
    .unwrap();
    route(
        json!({"event":"file_chooser","tab":1,"id":8,"mode":"open_folder","title":"Upload","accept":[]}),
    )
    .unwrap();
    route(json!({"event":"file_chooser","tab":1,"id":9,"mode":"open","title":"","accept":[]}))
        .unwrap();
    route(json!({"event":"file_chooser_closed","tab":1,"id":7})).unwrap();
    assert_eq!(
        engine.drain_events(),
        [
            WebViewEvent::FileChooser(FileChooser {
                id: 7,
                mode: FileChooserMode::Several,
                title: String::new(),
                accept: vec![".pdf".into(), "image/*".into()],
            }),
            WebViewEvent::FileChooser(FileChooser {
                id: 8,
                mode: FileChooserMode::Folder,
                title: "Upload".into(),
                accept: Vec::new(),
            }),
            WebViewEvent::FileChooser(FileChooser {
                id: 9,
                mode: FileChooserMode::One,
                title: String::new(),
                accept: Vec::new(),
            }),
            WebViewEvent::FileChooserClosed { id: 7 },
        ]
    );
    for malformed in [
        json!({"event":"file_chooser","tab":1,"id":0,"mode":"open","title":"","accept":[]}),
        json!({"event":"file_chooser","tab":1,"id":3,"mode":"save","title":"","accept":[]}),
        json!({"event":"file_chooser","tab":1,"id":3,"mode":"open","accept":[]}),
        json!({"event":"file_chooser","tab":1,"id":3,"mode":"open","title":"x".repeat(257),"accept":[]}),
        json!({"event":"file_chooser","tab":1,"id":3,"mode":"open","title":""}),
        json!({"event":"file_chooser","tab":1,"id":3,"mode":"open","title":"","accept":["pdf"]}),
        json!({"event":"file_chooser","tab":1,"id":3,"mode":"open","title":"","accept":[7]}),
        json!({"event":"file_chooser","tab":1,"id":3,"mode":"open","title":"",
            "accept":vec![".pdf"; 257]}),
        json!({"event":"file_chooser_closed","tab":1,"id":-1}),
    ] {
        assert!(route(malformed.clone()).is_err(), "{malformed}");
    }

    // Exactly the paths the person picked, in their order; none cancels.
    engine.send(WebViewCommand::AnswerFileChooser {
        id: 7,
        paths: vec!["/home/ada/one.png".into(), "/home/ada/two.png".into()],
    });
    assert_eq!(
        tx.pop().unwrap(),
        json!({"op":"file_chooser_reply","tab":1,"id":7,
            "paths":["/home/ada/one.png","/home/ada/two.png"]})
    );
    engine.send(WebViewCommand::AnswerFileChooser {
        id: 8,
        paths: Vec::new(),
    });
    assert_eq!(
        tx.pop().unwrap(),
        json!({"op":"file_chooser_reply","tab":1,"id":8,"paths":[]})
    );
    // What the host would end the engine over is never sent.
    for (id, paths) in [
        (0, vec!["/home/ada/proof.pdf".to_string()]),
        (9, vec!["proof.pdf".to_string()]),
        (
            9,
            vec!["/a".to_string(); crate::webview::file_chooser::MAX_FILE_CHOOSER_PATHS + 1],
        ),
    ] {
        engine.send(WebViewCommand::AnswerFileChooser { id, paths });
        assert_eq!(queued(), 0);
    }
}
