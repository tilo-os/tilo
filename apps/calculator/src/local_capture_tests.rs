// Exercise the surface's immediate-edit path with the dispatcher attached by
// the real framework. The queue is deliberately not connected to a broker:
// publishing an observation must not delay the document's local projection.
fn assert_calculator_capture(
    queue: &ui::local_document_action::Queue,
    doc: &mut AutoCommit,
    before: &[automerge::ChangeHash],
    expected: &Invoke<Calculator>,
) {
    let captures = queue.drain();
    assert_eq!(captures.len(), 1);
    let capture = &captures[0];
    assert_eq!(capture.revision, "calculator-pre-input-directory");
    let published = capture.published.as_ref().expect("canonical intent");
    assert_eq!(published.resource, "calculator.document");
    assert_eq!(published.action, expected.id);
    assert_eq!(published.arguments, expected.arguments);
    let mut original = before.iter().map(|hash| hash.0).collect::<Vec<_>>();
    original.sort_unstable();
    assert_eq!(capture.original_heads, original);
    let mut mutations = doc
        .get_changes(before)
        .iter()
        .map(|change| change.hash().0)
        .collect::<Vec<_>>();
    mutations.sort_unstable();
    assert!(!mutations.is_empty());
    assert_eq!(capture.mutation_hashes, mutations);
    assert!(queue.take_refusals().is_empty());
}

#[test]
fn local_capture_commit_equals_is_immediate_and_binds_actual_changes() {
    use ui::tilo_resource::Resource as _;

    let mut framework = hosted();
    let mut doc = framework.read_document(Clone::clone);
    let before = doc.get_heads();
    let queue = ui::local_document_action::Queue::new();
    // The compositor supplies this opaque, frozen directory revision in a
    // running app; this test owns the route scope and never sends a response.
    let _scope = queue.enter("calculator-pre-input-directory".into());
    let surface = framework.surface_mut::<CalculatorApp>();
    surface.model.expr = "6*7".into();
    surface.commit_equals(&mut Edits::lend(&mut doc));

    let stored = Calculator::read(&doc).unwrap();
    let (_, entry) = stored.history.iter().next().expect("visible before drain");
    assert_eq!((&*entry.expr, &*entry.result), ("6*7", "42"));
    assert_eq!(surface.model.ans, 42.0);
    assert_calculator_capture(
        &queue,
        &mut doc,
        &before,
        &Calculator::COMMIT_HISTORY.with(("6*7".to_owned(), "42".to_owned())),
    );

    let heads = doc.get_heads();
    for expr in ["", "sqrt("] {
        surface.model.expr = expr.into();
        surface.commit_equals(&mut Edits::lend(&mut doc));
        assert_eq!(doc.get_heads(), heads);
        assert!(
            queue.drain().is_empty(),
            "failed/empty equals is not an action"
        );
    }
    surface.set_mode(Mode::Graph2d);
    assert_eq!(
        doc.get_heads(),
        heads,
        "presentation is not a document edit"
    );
    assert!(queue.drain().is_empty());
}

#[test]
fn local_capture_set_graph_expr_keeps_the_canonical_field_edit() {
    use ui::framework::Submit as _;
    use ui::tilo_resource::Resource as _;

    let mut framework = hosted();
    framework
        .invoke(Calculator::ADD_FUNCTION.with("x".to_owned()))
        .unwrap();
    framework.settle();
    let id = framework
        .state()
        .graph_id(GraphKind::Curve2d, 0)
        .unwrap()
        .clone();
    let mut doc = framework.read_document(Clone::clone);
    let before = doc.get_heads();
    let queue = ui::local_document_action::Queue::new();
    let _scope = queue.enter("calculator-pre-input-directory".into());
    framework.surface_mut::<CalculatorApp>().set_graph_expr(
        GraphKind::Curve2d,
        0,
        "x*x+1",
        &mut Edits::lend(&mut doc),
    );

    assert_eq!(
        Calculator::read(&doc).unwrap().funcs.get(&id).unwrap().expr,
        "x*x+1",
        "the stored formula is readable without a broker response"
    );
    assert_calculator_capture(
        &queue,
        &mut doc,
        &before,
        &GraphKind::Curve2d
            .rows()
            .item(&id)
            .expr
            .replace("x*x+1")
            .into_invoke()
            .unwrap(),
    );
}
