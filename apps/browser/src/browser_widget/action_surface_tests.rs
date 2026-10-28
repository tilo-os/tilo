//! The window's controls as actions: what a person reaches by clicking in
//! the menu, a script, an agent and assistive technology reach on the
//! window's rows, through the same effect the window answers.

use super::*;

fn effect_operation(payload: &[u8]) -> BrowserOperation {
    use ui::tilo_resource::Effect as _;
    crate::effects::BrowserEffect::from_payload(payload)
        .expect("a browser effect")
        .operation()
}

/// Zoom is the page's to do for anyone: the menu's zoom rows are actions
/// on the page, each offered while it would change the page's size.
#[test]
fn the_page_zooms_through_its_actions() {
    let engine = ui::webview::test_engine::TestEngine::new();
    let mut widget = BrowserWidget::with_engine(engine.clone(), true);
    let zoom = |widget: &BrowserWidget, id: &str| {
        let published = widget.published();
        let page = published
            .children
            .iter()
            .find(|row| row.id.0 == "browser.page")
            .unwrap();
        let action = page.actions.iter().find(|action| action.id == id).unwrap();
        let available = matches!(
            action.contract.availability,
            ui::tilo_actions::Availability::Available
        );
        let encoded = action.build_json(&serde_json::Map::new()).unwrap();
        (available, effect_operation(&encoded.payload))
    };
    let run = |widget: &mut BrowserWidget, id: &str| {
        let (_, operation) = zoom(widget, id);
        match widget.admit(ui::tilo_actions::InvocationId::new(), operation) {
            Ok(ui::framework::Effected::Done(output)) => Ok(output.value),
            Ok(ui::framework::Effected::Pending) => panic!("{id} is done at once"),
            Err(error) => Err(error),
        }
    };
    assert!(zoom(&widget, "zoom-in").0 && zoom(&widget, "zoom-out").0);
    assert!(!zoom(&widget, "reset-zoom").0, "at its own size already");
    assert_eq!(
        run(&mut widget, "zoom-in"),
        Ok(Some(serde_json::json!("The page is at 120%")))
    );
    assert_eq!(
        engine.commands().last(),
        Some(&WebViewCommand::SetZoomLevel(1.0))
    );
    assert!(zoom(&widget, "reset-zoom").0);
    assert_eq!(
        run(&mut widget, "reset-zoom"),
        Ok(Some(serde_json::json!("The page is at 100%")))
    );
    assert_eq!(
        engine.commands().last(),
        Some(&WebViewCommand::SetZoomLevel(0.0))
    );
    for _ in 0..5 {
        run(&mut widget, "zoom-out").unwrap();
    }
    assert!(!zoom(&widget, "zoom-out").0, "at its smallest");
    assert!(run(&mut widget, "zoom-out").is_err());
}
