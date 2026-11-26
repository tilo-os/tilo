use crate::{widget::Checkbox, widget::Dropdown, widget::Slider, widget::Toggle};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT};
use tilo_ui_base::a11y::Action;
use tilo_ui_base::widget::Widget;

#[test]
fn value_controls_read_document_and_leave_persistence_to_actions() {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "checked", false).unwrap();
    doc.put(ROOT, "number", 0.25).unwrap();
    doc.put(ROOT, "choice", "first").unwrap();
    let mut controls: Vec<(Box<dyn Widget>, Action)> = vec![
        (
            Box::new(Checkbox::bound("check", "checked")),
            Action::Activate,
        ),
        (
            Box::new(Toggle::bound("toggle", "checked")),
            Action::Activate,
        ),
        (
            Box::new(Slider::new("slider", "number")),
            Action::SetValue("0.75".into()),
        ),
        (
            Box::new(Dropdown::new(
                vec!["first".into(), "second".into()],
                "choice",
            )),
            Action::SetValue("second".into()),
        ),
    ];
    let before = tilo_ui_base::doc::settle_mark(&doc);
    for (control, action) in &mut controls {
        control.sync(&doc, &ROOT);
        assert!(control.perform_action(
            action,
            &[],
            &mut tilo_ui_base::widget::Edits::lend(&mut doc),
            &ROOT
        ));
        assert_eq!(
            tilo_ui_base::doc::settle_mark(&doc),
            before,
            "a value control cannot write its read source"
        );
    }
}
