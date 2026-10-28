//! The page's input method: compositions forwarded to the engine, and the
//! caret the engine reports placing the input method's candidates.

use super::tests::{dispatch, laid_out};
use super::*;
use crate::webview::test_engine::TestEngine;
use automerge::AutoCommit;
use winit::event::WindowEvent;

#[test]
fn the_input_method_opens_where_the_page_reports_its_caret() {
    use crate::webview::TextField;
    use winit::event::Ime;
    let ime_commands = |engine: &TestEngine| -> Vec<WebViewCommand> {
        engine
            .commands()
            .into_iter()
            .filter(|command| {
                matches!(
                    command,
                    WebViewCommand::ImeComposition { .. }
                        | WebViewCommand::ImeCommit(_)
                        | WebViewCommand::ImeCancel
                        | WebViewCommand::ImeFinish
                )
            })
            .collect()
    };
    let engine = TestEngine::new();
    let (mut view, tree, node) = laid_out(engine.clone());
    let doc = AutoCommit::new();
    view.set_focus(true);
    // An engine that reports no field leaves the page to deferred text.
    assert_eq!(view.ime_cursor_rect(), None);
    assert!(view.ime_refuses_ordinary_commits());

    // A search field 20 px in and 40 px down, 200 by 30.
    let field = TextField {
        purpose: crate::ime::InputPurpose::Search,
        multiline: false,
        bounds: Some(Rect::new(20.0, 40.0, 220.0, 70.0)),
        composition: None,
    };
    engine.set_text_field(Some(field));
    assert!(!view.ime_refuses_ordinary_commits());
    assert_eq!(
        view.ime_content().map(|content| content.purpose),
        Some(crate::ime::InputPurpose::Search)
    );
    // Serving the input method grants no dictation: that needs a lease.
    assert_eq!(view.ime_edit_fingerprint(&[7; 32]), None);
    assert_eq!(
        view.ime_cursor_rect(),
        Some(Rect::new(20.0, 40.0, 220.0, 70.0))
    );

    // While composing, the caret is the composed text, and follows it.
    dispatch(
        &mut view,
        &tree,
        node,
        &WindowEvent::Ime(Ime::Preedit("にほん".into(), Some((9, 9)))),
    );
    let composing = |x: f64| TextField {
        composition: Some(Rect::new(x, 44.0, x + 48.0, 66.0)),
        ..field
    };
    engine.set_text_field(Some(composing(24.0)));
    assert_eq!(
        view.ime_cursor_rect(),
        Some(Rect::new(24.0, 44.0, 72.0, 66.0))
    );
    engine.set_text_field(Some(composing(60.0)));
    assert_eq!(
        view.ime_cursor_rect(),
        Some(Rect::new(60.0, 44.0, 108.0, 66.0))
    );

    // An emptied preedit followed by its commit is the commit alone: the
    // page's composition ends with the committed text.
    dispatch(
        &mut view,
        &tree,
        node,
        &WindowEvent::Ime(Ime::Preedit(String::new(), None)),
    );
    dispatch(
        &mut view,
        &tree,
        node,
        &WindowEvent::Ime(Ime::Commit("日本".into())),
    );
    view.sync(&doc, &automerge::ROOT);
    assert_eq!(
        ime_commands(&engine),
        vec![
            WebViewCommand::ImeComposition {
                text: "にほん".into(),
                cursor: Some((9, 9)),
            },
            WebViewCommand::ImeCommit("日本".into()),
        ]
    );
    // A stale composition box no longer counts once nothing composes.
    assert_eq!(
        view.ime_cursor_rect(),
        Some(Rect::new(20.0, 40.0, 220.0, 70.0))
    );

    // An emptied preedit nothing commits withdraws the composition.
    dispatch(
        &mut view,
        &tree,
        node,
        &WindowEvent::Ime(Ime::Preedit("x".into(), None)),
    );
    dispatch(
        &mut view,
        &tree,
        node,
        &WindowEvent::Ime(Ime::Preedit(String::new(), None)),
    );
    view.sync(&doc, &automerge::ROOT);
    assert_eq!(
        ime_commands(&engine).last(),
        Some(&WebViewCommand::ImeCancel)
    );

    // A tall field is placed by its first line, a field scrolled out of
    // sight by the view's corner, and a dense page in its own pixels.
    engine.set_text_field(Some(TextField {
        multiline: true,
        bounds: Some(Rect::new(10.0, 100.0, 390.0, 290.0)),
        ..field
    }));
    assert!(view.ime_multiline());
    assert_eq!(
        view.ime_cursor_rect(),
        Some(Rect::new(
            10.0,
            100.0,
            390.0,
            100.0 + ControlSize::Regular.height(&Theme::dark())
        ))
    );
    engine.set_text_field(Some(TextField {
        bounds: Some(Rect::new(10.0, 900.0, 390.0, 930.0)),
        ..field
    }));
    assert_eq!(
        view.ime_cursor_rect(),
        Some(Rect::new(0.0, 299.0, 1.0, 300.0))
    );

    // A password field never reaches the input method.
    engine.set_text_field(Some(TextField {
        purpose: crate::ime::InputPurpose::Password,
        ..field
    }));
    assert_eq!(view.ime_content(), None);
    assert_eq!(view.ime_cursor_rect(), None);

    // Nor does anything once the page loses the keyboard.
    engine.set_text_field(Some(field));
    view.set_focus(false);
    assert_eq!(view.ime_cursor_rect(), None);
}

#[test]
fn a_dense_page_reports_its_caret_in_its_own_pixels() {
    use crate::webview::TextField;
    let engine = TestEngine::new();
    let mut view = WebView::new(engine.clone()).with_scale_factor(2.0);
    view.set_viewport(Rect::new(100.0, 50.0, 500.0, 350.0), 2.0);
    view.set_focus(true);
    engine.set_text_field(Some(TextField {
        bounds: Some(Rect::new(40.0, 80.0, 440.0, 140.0)),
        ..TextField::default()
    }));
    assert_eq!(
        view.ime_cursor_rect(),
        Some(Rect::new(120.0, 90.0, 320.0, 120.0))
    );
}
