//! The effective-invocation witness the shared directory snapshot
//! ([`collaboration::action_directory`]) computes, checked against the
//! encoders the toolkit's widgets actually generate.
mod tests {
    use ::collaboration::action_directory::ActionDirectorySnapshot;
    use tilo_actions::{
        ActionSpec, ContinuationEvidence, ContinuationKey, ContinuationQuery, EncodedAction,
        ExecutionLocus, OperationKind, ResourceSpec,
    };
    fn fixed(value: usize) -> ActionSpec {
        ActionSpec::operation("run", "Run", OperationKind::View, "test.effective", &value).unwrap()
    }
    fn witness(spec: ActionSpec) -> Option<[u8; 32]> {
        let resources = vec![ResourceSpec::new("probe", "probe", "Probe").with_action(spec)];
        let descriptors = tilo_actions::door::survey(&resources);
        ActionDirectorySnapshot::build(&resources, &descriptors, |_| None).effective_invocations
    }
    #[test]
    fn semantic_set_value_continuation_covers_actual_generated_effective_snapshot() {
        use crate::widget::Widget;
        let mut field =
            crate::widget::EditableLabel::new("Original").with_accessible_name("Page title");
        field.set_semantic_target(Some("notes:page-title:page-a".into()));
        let mut taffy = taffy::TaffyTree::<()>::new();
        let mut map = std::collections::HashMap::new();
        let root = field.build_layout(&mut taffy, &crate::theme::Theme::dark(), &mut map);
        taffy
            .compute_layout(
                root,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(800.),
                    height: taffy::AvailableSpace::Definite(600.),
                },
            )
            .unwrap();
        field.apply_layout(&taffy, root, vello::kurbo::Point::ZERO);
        let mut tree = crate::a11y_tree::SemanticTree::new();
        let mut project = |field: &crate::widget::EditableLabel| {
            tree.refresh(field, &map, None, &[]);
            let resources = tree.action_resources();
            let descriptors = tilo_actions::door::survey(&resources);
            ActionDirectorySnapshot::build(&resources, &descriptors, |_| None).effective_invocations
        };
        let baseline = project(&field).expect("actual generated SetValue supports comparison");
        assert_eq!(project(&field), Some(baseline));
        field.set_semantic_target(Some("notes:page-title:page-b".into()));
        assert_ne!(project(&field), Some(baseline));
    }

    #[test]
    fn directory_effective_stream_binds_both_actual_fixed_lanes_and_locus() {
        let original = fixed(1);
        let baseline = witness(original.clone()).expect("actual Fixed builders support comparison");
        assert_eq!(witness(original.clone()), Some(baseline));
        assert_ne!(witness(fixed(2)), Some(baseline));
        let public = original
            .clone()
            .with_immediate(EncodedAction::encode_operation("test.other", &1).unwrap());
        assert_ne!(witness(public), Some(baseline));
        let mut operation_type = original.clone();
        operation_type.type_mut().operation_type = Some("test.other".into());
        assert_ne!(witness(operation_type), Some(baseline));
        let mut locus = original;
        locus.type_mut().locus = ExecutionLocus::ServerPreferred;
        assert_ne!(witness(locus), Some(baseline));
    }
    fn dynamic(domain: &'static [u8], handler: &'static [u8]) -> ActionSpec {
        ActionSpec::prompted_operation(
            "run",
            "Run",
            "Value",
            "",
            OperationKind::View,
            "test.effective",
            |text| EncodedAction::encode_operation("test.effective", &text).unwrap(),
        )
        .with_continuation_identity(move |query| {
            matches!(query, ContinuationQuery::Draft).then(|| ContinuationEvidence::Draft {
                resolution_domain: ContinuationKey::new("domain", [domain]),
                handler: ContinuationKey::new("handler", [handler]),
            })
        })
    }
    #[test]
    fn directory_effective_stream_binds_prompted_domain_and_handler_without_encoding() {
        let base = witness(dynamic(b"one", b"v1")).expect("explicit draft identity");
        assert_eq!(witness(dynamic(b"one", b"v1")), Some(base));
        assert_ne!(witness(dynamic(b"two", b"v1")), Some(base));
        assert_ne!(witness(dynamic(b"one", b"v2")), Some(base));
    }
    #[test]
    fn directory_effective_stream_unknown_or_lost_evidence_refuses() {
        let plain = ActionSpec::typed_operation(
            "run",
            "Run",
            "",
            OperationKind::View,
            "test.effective",
            vec![],
            |_| EncodedAction::encode_operation("test.effective", &1).unwrap(),
        );
        assert!(
            witness(plain).is_none(),
            "typed-zero is opaque despite its Immediate palette projection"
        );
        assert!(witness(fixed(1).with_continuation_identity(|_| None)).is_none());
        assert!(witness(dynamic(b"one", b"v1").with_continuation_identity(|_| None)).is_none());
    }
    #[test]
    fn directory_effective_stream_supported_large_materialization_and_duplicates() {
        let mut resource = ResourceSpec::new("probe", "probe", "Probe");
        for index in 0..600 {
            let mut spec = fixed(index);
            spec.type_mut().id = index.to_string();
            resource.actions.push(spec);
        }
        resource.actions.push(
            ActionSpec::operation(
                "large",
                "Large",
                OperationKind::View,
                "test.effective",
                &"x".repeat(70_000),
            )
            .unwrap(),
        );
        let resources = vec![resource];
        let descriptors = tilo_actions::door::survey(&resources);
        assert!(
            ActionDirectorySnapshot::build(&resources, &descriptors, |_| None)
                .effective_invocations
                .is_some()
        );
        let mut duplicated = resources;
        let duplicate = duplicated[0].actions[0].clone();
        duplicated[0].actions.push(duplicate);
        let descriptors = tilo_actions::door::survey(&duplicated);
        assert!(
            ActionDirectorySnapshot::build(&duplicated, &descriptors, |_| None)
                .effective_invocations
                .is_none()
        );
    }
}
