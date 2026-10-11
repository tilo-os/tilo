//! Fixed-retention comparison of actual materialized invocation encoders.
//! This is comparison evidence, never permission or a document-basis witness.
use tilo_actions::{
    ActionInput, ActionSpec, ContinuationArguments, ContinuationEvidence, ContinuationQuery,
    EncodedAction, ExecutionLocus, Resource,
};

pub(super) struct Builder {
    digest: blake3::Hasher,
    supported: bool,
    actions: usize,
}

impl Builder {
    pub(super) fn new() -> Self {
        let mut digest = blake3::Hasher::new();
        digest.update(b"tilo.directory-effective-invocations\0v1");
        Self {
            digest,
            supported: true,
            actions: 0,
        }
    }

    fn tag(&mut self, value: u8) {
        self.digest.update(&[value]);
    }
    fn length(&mut self, value: usize) {
        match u64::try_from(value) {
            Ok(value) => {
                self.digest.update(&value.to_le_bytes());
            }
            Err(_) => self.supported = false,
        }
    }
    fn bytes(&mut self, value: &[u8]) {
        self.length(value.len());
        self.digest.update(value);
    }
    fn encoded(&mut self, value: &EncodedAction) {
        self.bytes(value.type_id.as_bytes());
        self.bytes(&value.payload);
    }
    fn evidence(&mut self, value: Option<&ContinuationEvidence>) {
        struct Sink<'a>(&'a mut blake3::Hasher);
        impl std::io::Write for Sink<'_> {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.update(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        match value {
            Some(value) => {
                if value
                    .write_canonical_identity(&mut Sink(&mut self.digest))
                    .is_err()
                {
                    self.supported = false;
                }
            }
            None => self.supported = false,
        }
    }

    pub(super) fn resource(&mut self, resource: &Resource) {
        self.tag(0);
        self.bytes(resource.id.as_str().as_bytes());
        self.length(resource.actions.len());
        self.length(resource.children.len());
    }

    pub(super) fn action(&mut self, resource: &str, action: &ActionSpec) {
        let Some(count) = self.actions.checked_add(1) else {
            self.supported = false;
            return;
        };
        self.actions = count;
        self.tag(1);
        self.bytes(resource.as_bytes());
        self.bytes(action.id.as_bytes());
        self.tag(match action.locus {
            ExecutionLocus::ClientLocal => 0,
            ExecutionLocus::ServerPreferred => 1,
        });
        match &action.operation_type {
            Some(value) => {
                self.tag(1);
                self.bytes(value.as_bytes());
            }
            None => self.tag(0),
        }

        // Query an opaque builder's draft once, without invoking either
        // encoder. Its owner must explicitly cover the whole resolution domain.
        let fixed = action.fixed_invocation_encoding();
        let needs_draft = fixed.is_none() || matches!(action.input, ActionInput::Prompted { .. });
        let draft = needs_draft
            .then(|| action.continuation_identity(ContinuationQuery::Draft))
            .flatten();
        if let Some(encoded) = fixed {
            self.tag(0);
            self.encoded(encoded);
            if action.has_continuation_identity() {
                self.tag(1);
                let exact = action.continuation_identity(ContinuationQuery::Exact(
                    ContinuationArguments::Immediate,
                ));
                self.evidence(exact.as_ref());
            } else {
                // The actual bound registry / explicit owner domain supplies
                // handler stability. Absence is not an indirect-target claim.
                self.tag(0);
            }
        } else {
            self.tag(1);
            match action.deferred_invocation_type() {
                Some(type_id) => {
                    self.tag(1);
                    self.bytes(type_id.as_bytes());
                }
                None => self.tag(0),
            }
            self.evidence(draft.as_ref());
        }

        // Legacy door::encode uses this public lane directly. Do not assume it
        // agrees with the private builder: callers can replace the public input.
        match &action.input {
            ActionInput::Immediate(encoded) => {
                self.tag(0);
                self.encoded(encoded);
            }
            ActionInput::Prompted { .. } => {
                self.tag(1);
                self.evidence(draft.as_ref());
            }
            ActionInput::Deferred { type_id, .. } => {
                self.tag(2);
                self.bytes(type_id.as_bytes());
                if action.validate_json(&serde_json::Map::new()).is_err() {
                    self.supported = false;
                }
                self.evidence(draft.as_ref());
            }
        }
    }

    pub(super) fn finish(mut self, unique_actions: usize) -> Option<[u8; 32]> {
        // Reuse the publication's unique-address map to refuse duplicates;
        // there is no second map or retained copy of payloads/evidence here.
        if !self.supported || self.actions != unique_actions {
            return None;
        }
        self.tag(2);
        self.length(self.actions);
        self.supported.then(|| *self.digest.finalize().as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::action_directory_snapshot::ActionDirectorySnapshot;
    use tilo_actions::{
        ActionInput, ContinuationEvidence, ContinuationKey, ContinuationQuery, EncodedAction,
        ExecutionLocus, OperationKind,
    };
    fn fixed(value: usize) -> ActionSpec {
        ActionSpec::operation("run", "Run", OperationKind::View, "test.effective", &value).unwrap()
    }
    fn witness(spec: ActionSpec) -> Option<[u8; 32]> {
        let resources = vec![Resource::new("probe", "probe", "Probe").with_action(spec)];
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
        let mut public = original.clone();
        public.input = ActionInput::Immediate(
            EncodedAction::encode_operation("test.other", &1)
                .unwrap()
                .into(),
        );
        assert_ne!(witness(public), Some(baseline));
        let mut operation_type = original.clone();
        operation_type.operation_type = Some("test.other".into());
        assert_ne!(witness(operation_type), Some(baseline));
        let mut locus = original;
        locus.locus = ExecutionLocus::ServerPreferred;
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
        let mut resource = Resource::new("probe", "probe", "Probe");
        for index in 0..600 {
            let mut spec = fixed(index);
            spec.id = index.to_string();
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
