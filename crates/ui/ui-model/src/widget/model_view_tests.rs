//! A model view over a window's state: it builds its subtree again when the state changes, undo
//! included, without the app telling it.

use automerge::{ROOT, ReadDoc, ScalarValue, Value, transaction::Transactable};
use tilo_ui::widget::{ModelView, Text};
use tilo_ui_base::widget::Widget;

fn string(doc: &automerge::AutoCommit, key: &str) -> String {
    match doc.get(&ROOT, key) {
        Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
            ScalarValue::Str(value) => value.to_string(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

tilo_test_env::scenario! {
    fn a_model_view_follows_its_state;

    phase document_view_tracks_undo_without_app_owned_notifications;
    {
        let mut state = crate::State::new();
        state.doc.put(&ROOT, "mode", "list").unwrap();
        state.commit_action(None, None);
        let mut view = ModelView::from_document(
            |doc, _| string(doc, "mode"),
            |mode| Box::new(Text::new(mode)),
        )
        .autofocus_on_change(true);

        view.sync(&state.doc, &ROOT);
        assert_eq!(
            view.children()[0].accessibility().unwrap().name.as_deref(),
            Some("list")
        );
        assert!(view.take_focus_reset());

        state.doc.put(&ROOT, "mode", "detail").unwrap();
        state.commit_action(None, None);
        view.sync(&state.doc, &ROOT);
        assert_eq!(
            view.children()[0].accessibility().unwrap().name.as_deref(),
            Some("detail")
        );

        state.undo(None, None).unwrap();
        view.sync(&state.doc, &ROOT);
        assert_eq!(
            view.children()[0].accessibility().unwrap().name.as_deref(),
            Some("list")
        );
    }
}
