//! Fixed-retention comparison of actual materialized invocation encoders.
//! This is comparison evidence, never permission or a document-basis witness.
use tilo_actions::{
    ActionInput, ActionSpec, ContinuationArguments, ContinuationEvidence, ContinuationQuery,
    EncodedAction, ExecutionLocus, ResourceSpec,
};

pub(crate) struct Builder {
    digest: blake3::Hasher,
    supported: bool,
    actions: usize,
}

impl Builder {
    pub(crate) fn new() -> Self {
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
        match value {
            Some(value) => {
                if value.write_canonical_identity(&mut self.digest).is_err() {
                    self.supported = false;
                }
            }
            None => self.supported = false,
        }
    }

    pub(crate) fn resource(&mut self, resource: &ResourceSpec) {
        self.tag(0);
        self.bytes(resource.id.as_str().as_bytes());
        self.length(resource.actions.len());
        self.length(resource.children.len());
    }

    pub(crate) fn action(&mut self, resource: &str, action: &ActionSpec) {
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
        let needs_draft = fixed.is_none() || matches!(action.input(), ActionInput::Prompted { .. });
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

        // How door::encode collects the input: the declared encoding, or the
        // builder bound to the spec's target, whose draft covers it.
        match action.input() {
            ActionInput::Immediate(Some(encoded)) => {
                self.tag(0);
                self.encoded(encoded);
            }
            ActionInput::Immediate(None) => {
                self.tag(3);
                self.evidence(draft.as_ref());
            }
            ActionInput::Prompted { .. } => {
                self.tag(1);
                self.evidence(draft.as_ref());
            }
            ActionInput::Deferred { type_id } => {
                self.tag(2);
                self.bytes(type_id.as_bytes());
                if action.validate_json(&serde_json::Map::new()).is_err() {
                    self.supported = false;
                }
                self.evidence(draft.as_ref());
            }
        }
    }

    pub(crate) fn finish(mut self, unique_actions: usize) -> Option<[u8; 32]> {
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
