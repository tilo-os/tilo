use super::*;

#[test]
fn a_pages_popup_is_routed_from_the_moment_it_opens_until_it_closes() {
    let opener = Arc::new(Shared::default());
    let routes = Arc::new(Mutex::new(HashMap::from([(1, Arc::downgrade(&opener))])));
    let downloads = Mutex::new(VecDeque::new());
    let (ready, _receiver) = sync_channel(1);
    let popup = FIRST_POPUP + 3;
    let opened = json!({"event":"popup","tab":1,"popup":popup,"url":"https://accounts.example/","width":420,"height":520});
    route_native_event(&opened, &routes, &downloads, &ready).unwrap();
    assert_eq!(
        opener
            .popups
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
        vec![crate::webview::Popup {
            id: popup,
            url: "https://accounts.example/".into(),
            width: 420,
            height: 520,
        }]
    );
    // Its first events reach it before the app adopts it.
    route_native_event(
        &json!({"event":"title","tab":popup,"title":"Sign in"}),
        &routes,
        &downloads,
        &ready,
    )
    .unwrap();
    let endpoint = opener
        .unadopted
        .lock()
        .unwrap()
        .get(&popup)
        .cloned()
        .unwrap();
    assert!(
        endpoint
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event, WebViewEvent::TitleChanged(title) if title == "Sign in"))
    );
    assert!(
        opener
            .events
            .lock()
            .unwrap()
            .iter()
            .all(|event| !matches!(event, WebViewEvent::TitleChanged(title) if title == "Sign in"))
    );
    // Closed before it was adopted, it no longer counts against the bound.
    route_native_event(
        &json!({"event":"closed","tab":popup}),
        &routes,
        &downloads,
        &ready,
    )
    .unwrap();
    drop(endpoint);
    for n in 0..8 {
        let next = json!({"event":"popup","tab":1,"popup":popup + 1 + n,"url":"https://a.example/","width":0,"height":0});
        route_native_event(&next, &routes, &downloads, &ready).unwrap();
    }
    assert_eq!(opener.unadopted.lock().unwrap().len(), 8);
    assert!(!opener.unadopted.lock().unwrap().contains_key(&popup));
    // A popup id among the app's own tabs is refused.
    let forged =
        json!({"event":"popup","tab":1,"popup":2,"url":"https://a.example/","width":0,"height":0});
    assert!(route_native_event(&forged, &routes, &downloads, &ready).is_err());
}
