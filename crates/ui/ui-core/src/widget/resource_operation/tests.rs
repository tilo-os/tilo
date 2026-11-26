use tilo_ui_base::widget::resource_operation::*;
use crate::widget::Button;
use tilo_ui_base::widget::Decorator;
use automerge::transaction::Transactable;
use automerge::ObjId;

/// Answers `"probe.write"` by writing `written` into the document.
struct Writer {
    inner: Button,
}

impl Decorator for Writer {
    tilo_ui_base::provides!(dyn tilo_ui_base::widget::ResourceOperations; inner = inner);
    type Inner = Button;

    fn inner(&self) -> &Button {
        &self.inner
    }

    fn inner_mut(&mut self) -> &mut Button {
        &mut self.inner
    }
}

impl tilo_ui_base::widget::ResourceOperations for Writer {
    fn start_operation(
        &mut self,
        op: &ResourceOperation<'_>,
        doc: &mut tilo_ui_base::widget::Edits<'_>,
        _: &ObjId,
    ) -> Result<Option<ResourceOperationStart>, String> {
        if op.request().type_id != "probe.write" {
            return Ok(None);
        }
        doc.document_mut()
            .put(automerge::ROOT, "written", true)
            .map_err(|error| error.to_string())?;
        Ok(Some(ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        )))
    }
}

fn offer(
    widget: &mut Writer,
    doc: &mut tilo_ui_base::widget::Edits<'_>,
    type_id: &str,
) -> Result<bool, String> {
    let op = ResourceOperation::local(
        tilo_actions::InvocationId::new(),
        tilo_actions::OperationKind::View,
        type_id,
        b"null",
    );
    start_without_document_writes(widget, &op, doc, &automerge::ROOT).map(|s| s.is_some())
}

#[test]
fn a_read_only_caller_keeps_its_open_edits_and_refuses_a_writing_handler() {
    let mut widget = Writer {
        inner: Button::new("probe", "Probe"),
    };
    let mut doc = automerge::AutoCommit::new();
    doc.put(automerge::ROOT, "typing", "pending").unwrap();
    let pending = doc.pending_ops();

    assert_eq!(
        offer(
            &mut widget,
            &mut tilo_ui_base::widget::Edits::lend(&mut doc),
            "probe.other"
        ),
        Ok(false)
    );
    assert_eq!(
        doc.pending_ops(),
        pending,
        "observing the document must not commit the caller's open transaction"
    );

    let refused = offer(
        &mut widget,
        &mut tilo_ui_base::widget::Edits::lend(&mut doc),
        "probe.write",
    );
    assert!(refused.is_err(), "a handler that wrote must be refused");
    assert!(
        doc.pending_ops() > pending,
        "the caller's own edits are retained rather than rolled back"
    );
}
